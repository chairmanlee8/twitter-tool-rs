mod bottom_bar;
mod tweet_pane;
mod tweets_pane;

use crate::twitter_client::{api, TwitterClient};
use crate::ui::bottom_bar::render_bottom_bar;
use crate::ui::tweet_pane::render_tweet_pane;
use crate::ui::tweets_pane::render_tweets_pane;
use anyhow::{anyhow, Context, Error, Result};
use crossterm::cursor;
use crossterm::event::{Event, EventStream, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size, Clear, ClearType};
use crossterm::{
    execute, queue,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::{FutureExt, StreamExt};
use std::cmp::{max, min};
use std::collections::HashMap;
use std::fs;
use std::io::{stdout, Write};
use std::process;
use std::sync::Arc;
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    Mutex,
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Mode {
    Log,
    Interactive,
}

pub struct Layout {
    pub screen_cols: u16,
    pub screen_rows: u16,
    // TODO pub tweets_feed_pane_width, pub tweet_detail_pane_width, rename bottom_bar to status_bar
}

#[derive(Debug)]
enum InternalEvent {
    TweetsFeedUpdated,
    LogError(Error),
}

// TODO deep dive into str vs String
pub struct UI {
    mode: Mode,
    layout: Layout,
    events: (
        UnboundedSender<InternalEvent>,
        UnboundedReceiver<InternalEvent>,
    ),
    twitter_client: Arc<TwitterClient>,
    twitter_user: Arc<api::User>,
    tweets: Arc<Mutex<HashMap<String, api::Tweet>>>,
    tweets_reverse_chronological: Arc<Mutex<Vec<String>>>,
    tweets_page_token: Arc<Mutex<Option<String>>>,
    tweets_view_offset: usize,
    tweets_selected_index: usize,
    tweet_pane_width: u16,
}

impl UI {
    pub fn new(twitter_client: TwitterClient, twitter_user: api::User) -> Self {
        let (cols, rows) = size().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();

        Self {
            mode: Mode::Log,
            layout: Layout {
                screen_cols: cols,
                screen_rows: rows,
            },
            events: (tx, rx),
            twitter_client: Arc::new(twitter_client),
            twitter_user: Arc::new(twitter_user),
            tweets: Arc::new(Mutex::new(HashMap::new())),
            tweets_reverse_chronological: Arc::new(Mutex::new(Vec::new())),
            tweets_page_token: Arc::new(Mutex::new(None)),
            tweets_view_offset: 0,
            tweets_selected_index: 0,
            tweet_pane_width: 80,
        }
    }

    fn set_mode(&mut self, mode: Mode) -> Result<()> {
        let prev_mode = self.mode;
        self.mode = mode;

        if prev_mode == Mode::Log && mode == Mode::Interactive {
            execute!(stdout(), EnterAlternateScreen)?;
            enable_raw_mode()?;
        } else if prev_mode == Mode::Interactive && mode == Mode::Log {
            execute!(stdout(), LeaveAlternateScreen)?;
            enable_raw_mode()?;
            // CR: disabling raw mode entirely also gets rid of the keypress events...
            // disable_raw_mode()?;
        }

        Ok(())
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.layout = Layout {
            screen_cols: cols,
            screen_rows: rows,
        };
    }

    pub async fn move_selected_index(&mut self, delta: isize) -> Result<()> {
        let tweets_reverse_chronological = self.tweets_reverse_chronological.lock().await;

        let new_index = max(0, self.tweets_selected_index as isize + delta) as usize;
        let new_index = min(new_index, tweets_reverse_chronological.len() - 1);
        let view_top = self.tweets_view_offset;
        let view_height = (self.layout.screen_rows - 3) as usize;
        let view_bottom = self.tweets_view_offset + view_height;

        self.tweets_selected_index = new_index;

        drop(tweets_reverse_chronological);

        if new_index < view_top {
            self.tweets_view_offset = new_index;
            self.show_tweets().await
        } else if new_index > view_bottom {
            self.tweets_view_offset = max(0, new_index - view_height);
            self.show_tweets().await
        } else {
            // CR: this is confusing re the above two conditions, consider a refactor
            {
                let tweets = self.tweets.lock().await;
                let tweets_reverse_chronological = self.tweets_reverse_chronological.lock().await;

                render_tweet_pane(
                    &self.layout,
                    self.tweet_pane_width,
                    &tweets[&tweets_reverse_chronological[self.tweets_selected_index]],
                )?;

                render_bottom_bar(
                    &self.layout,
                    &tweets_reverse_chronological,
                    self.tweets_selected_index,
                )?;
            }

            execute!(
                stdout(),
                cursor::MoveTo(
                    16,
                    (self.tweets_selected_index - self.tweets_view_offset) as u16
                )
            )?;

            Ok(())
        }
    }

    // CR: switch to render_tweets (show_tweets then just sets state)
    pub async fn show_tweets(&mut self) -> Result<()> {
        self.set_mode(Mode::Interactive)?;

        queue!(stdout(), Clear(ClearType::All))?;

        let tweets = self.tweets.lock().await;
        let tweets_reverse_chronological = self.tweets_reverse_chronological.lock().await;

        render_tweets_pane(
            &self.layout,
            self.layout.screen_cols - self.tweet_pane_width - 2,
            &tweets,
            &tweets_reverse_chronological,
            self.tweets_view_offset,
        )?;

        render_tweet_pane(
            &self.layout,
            self.tweet_pane_width,
            &tweets[&tweets_reverse_chronological[self.tweets_selected_index]],
        )?;

        render_bottom_bar(
            &self.layout,
            &tweets_reverse_chronological,
            self.tweets_selected_index,
        )?;

        drop(tweets_reverse_chronological);
        drop(tweets);

        queue!(
            stdout(),
            cursor::MoveTo(
                16,
                (self.tweets_selected_index - self.tweets_view_offset) as u16
            )
        )?;

        stdout().flush()?;
        Ok(())
    }

    pub async fn log_selected_tweet(&mut self) -> Result<()> {
        {
            let tweets = self.tweets.lock().await;
            let tweets_reverse_chronological = self.tweets_reverse_chronological.lock().await;
            let tweet_id = &tweets_reverse_chronological[self.tweets_selected_index];
            let tweet = &tweets[tweet_id];
            fs::write("/tmp/tweet", format!("{:#?}", tweet))?;
        }

        let mut subshell = process::Command::new("less").args(["/tmp/tweet"]).spawn()?;
        subshell.wait()?;
        Ok(())
    }

    pub fn log_message(&mut self, message: &str) -> Result<()> {
        self.set_mode(Mode::Log)?;
        println!("{message}");
        Ok(())
    }

    pub fn do_load_page_of_tweets(&mut self, restart: bool) {
        let event_sender = self.events.0.clone();
        let twitter_client = self.twitter_client.clone();
        let twitter_user = self.twitter_user.clone();
        let tweets_page_token = self.tweets_page_token.clone();
        let tweets = self.tweets.clone();
        let tweets_reverse_chronological = self.tweets_reverse_chronological.clone();

        tokio::spawn(async move {
            match async {
                let mut tweets_page_token = tweets_page_token
                    .try_lock()
                    .with_context(|| "Cannot get lock")?;
                // NB: require page token if continuing to next page
                let maybe_page_token = match restart {
                    true => Ok::<Option<&String>, Error>(None),
                    false => {
                        let page_token =
                            tweets_page_token.as_ref().ok_or(anyhow!("No more pages"))?;
                        Ok(Some(page_token))
                    }
                }?;
                let (new_tweets, page_token) = twitter_client
                    .timeline_reverse_chronological(&twitter_user.id, maybe_page_token)
                    .await?;
                let mut new_tweets_reverse_chronological: Vec<String> = Vec::new();

                *tweets_page_token = page_token;

                {
                    let mut tweets = tweets.lock().await;
                    for tweet in new_tweets {
                        new_tweets_reverse_chronological.push(tweet.id.clone());
                        tweets.insert(tweet.id.clone(), tweet);
                    }
                }
                {
                    let mut tweets_reverse_chronological =
                        tweets_reverse_chronological.lock().await;
                    tweets_reverse_chronological.append(&mut new_tweets_reverse_chronological);
                }
                Ok(())
            }
            .await
            {
                Ok(()) => event_sender.send(InternalEvent::TweetsFeedUpdated),
                Err(error) => event_sender.send(InternalEvent::LogError(error)),
            }
        });
    }

    pub async fn event_loop(&mut self) -> Result<()> {
        let mut terminal_event_stream = EventStream::new();

        loop {
            let terminal_event = terminal_event_stream.next().fuse();
            let internal_event = self.events.1.recv();

            tokio::select! {
                maybe_event = terminal_event => {
                    match maybe_event {
                        Some(Ok(event)) => {
                            match event {
                                Event::Key(key_event) => match key_event.code {
                                    KeyCode::Esc => self.show_tweets().await?,
                                    KeyCode::Up => self.move_selected_index(-1).await?,
                                    KeyCode::Down => self.move_selected_index(1).await?,
                                    KeyCode::Char('h') => self.log_message("hello")?,
                                    KeyCode::Char('i') => self.log_selected_tweet().await?,
                                    KeyCode::Char('n') => {
                                        self.do_load_page_of_tweets(false);
                                    },
                                    KeyCode::Char('q') => {
                                        reset();
                                        process::exit(0);
                                    }
                                    _ => (),
                                },
                                Event::Resize(cols, rows) => self.resize(cols, rows),
                                _ => (),
                            }
                        }
                        _ => ()
                    }
                },
                ievent = internal_event => {
                    match ievent {
                        Some(InternalEvent::TweetsFeedUpdated) => {
                            self.show_tweets().await?;
                        },
                        Some(InternalEvent::LogError(err)) => {
                            self.log_message(err.to_string().as_str())?;
                        },
                        None => ()
                    }
                }
            }
        }
    }
}

pub fn reset() {
    execute!(stdout(), LeaveAlternateScreen).unwrap();
    disable_raw_mode().unwrap()
}
