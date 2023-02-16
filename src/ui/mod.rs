mod bottom_bar;
mod feed_pane;
mod tweet_pane;

use std::borrow::BorrowMut;
use crate::twitter_client::{api, TwitterClient};
use crate::ui::bottom_bar::BottomBar;
use crate::ui::feed_pane::FeedPane;
use crate::ui::tweet_pane::TweetPane;
use anyhow::{anyhow, Context, Error, Result};
use crossterm::cursor;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent};
use crossterm::terminal;
use crossterm::{
    execute, queue,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::{FutureExt, StreamExt};
//use std::cmp::{max, min};
use std::collections::HashMap;
use std::fs;
use std::io::{stdout, Stdout, Write};
use std::process;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex};
use tokio::sync::{mpsc::{self, UnboundedReceiver, UnboundedSender}};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Mode {
    Log,
    Interactive,
}

pub struct Layout {
    pub stdout: Stdout,
    pub screen_cols: u16,
    pub screen_rows: u16,
    pub feed_pane_width: u16,
    pub tweet_pane_width: u16,
}

#[derive(Debug)]
pub enum InternalEvent {
    FeedUpdated,
    LogError(Error),
}

pub trait Render {
    // NB: [render] takes [&mut self] since there isn't a separate notification to component that
    // their bbox changed
    fn render(&mut self, stdout: &mut Stdout, left: u16, top: u16, width: u16, height: u16) -> Result<()>;
}

pub trait Input {
    fn handle_key_event(&mut self, event: KeyEvent);
    fn get_cursor(&self) -> (u16, u16);
}

// CR-someday: pub trait Animate

pub trait Component: Render + Input {}

struct ShouldRender<T: Component> {
    pub should_render: bool,
    pub component: T
}

impl<T: Component> ShouldRender<T> {
    pub fn new(component: T) -> Self {
        ShouldRender {
            should_render: true,
            component,
        }
    }
}

// TODO deep dive into str vs String
pub struct UI {
    mode: Mode,
    layout: Layout,
    events: (UnboundedSender<InternalEvent>, UnboundedReceiver<InternalEvent>),
    feed_pane: ShouldRender<FeedPane>,
    focus_index: usize,
    twitter_client: Arc<TwitterClient>,
    twitter_user: Arc<api::User>,
    tweets: Arc<Mutex<HashMap<String, api::Tweet>>>,
    tweets_reverse_chronological: Arc<Mutex<Vec<String>>>,
    tweets_page_token: Arc<AsyncMutex<Option<String>>>,
    tweets_view_offset: usize,
    tweets_selected_index: usize,
    // CR-someday: maybe use Weak<dyn Input> here, but it runs into a gnarly type error
    // focus: Rc<dyn Input>,
    // feed_pane: ShouldRender<Rc<FeedPane>>,
    // tweet_pane: ShouldRender<Rc<TweetPane>>,
    // bottom_bar: ShouldRender<Rc<BottomBar>>,
}

impl UI {
    pub fn new(twitter_client: TwitterClient, twitter_user: api::User) -> Self {
        let (cols, rows) = terminal::size().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();

        let tweets = Arc::new(Mutex::new(HashMap::new()));
        let tweets_reverse_chronological = Arc::new(Mutex::new(Vec::new()));

        let feed_pane = FeedPane::new(&tx, &tweets, &tweets_reverse_chronological);
        let tweet_pane = TweetPane;
        let bottom_bar = BottomBar;

        Self {
            mode: Mode::Log,
            layout: Layout {
                stdout: stdout(),
                screen_cols: cols,
                screen_rows: rows,
                feed_pane_width: cols / 2,
                tweet_pane_width: cols / 2,
            },
            events: (tx, rx),
            feed_pane: ShouldRender::new(feed_pane),
            focus_index: 0,
            twitter_client: Arc::new(twitter_client),
            twitter_user: Arc::new(twitter_user),
            tweets,
            tweets_reverse_chronological,
            tweets_page_token: Arc::new(AsyncMutex::new(None)),
            tweets_view_offset: 0,
            tweets_selected_index: 0,
        }
    }

    fn set_mode(&mut self, mode: Mode) -> Result<()> {
        let prev_mode = self.mode;
        self.mode = mode;

        if prev_mode == Mode::Log && mode == Mode::Interactive {
            execute!(stdout(), EnterAlternateScreen)?;
            terminal::enable_raw_mode()?;
        } else if prev_mode == Mode::Interactive && mode == Mode::Log {
            execute!(stdout(), LeaveAlternateScreen)?;
            terminal::enable_raw_mode()?;
            // CR: disabling raw mode entirely also gets rid of the keypress events...
            // disable_raw_mode()?;
        }

        Ok(())
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.layout.screen_cols = cols;
        self.layout.screen_rows = rows;
    }

    // pub async fn move_selected_index(&mut self, delta: isize) -> Result<()> {
    //     {
    //         let tweets_reverse_chronological = self.tweets_reverse_chronological.lock().await;
    //
    //         let new_index = max(0, self.tweets_selected_index as isize + delta) as usize;
    //         let new_index = min(new_index, tweets_reverse_chronological.len() - 1);
    //         let view_top = self.tweets_view_offset;
    //         let view_height = (self.layout.screen_rows - 3) as usize;
    //         let view_bottom = self.tweets_view_offset + view_height;
    //
    //         self.tweets_selected_index = new_index;
    //
    //         if new_index < view_top {
    //             self.tweets_view_offset = new_index;
    //             self.feed_pane.should_render = true;
    //         } else if new_index > view_bottom {
    //             self.tweets_view_offset = max(0, new_index - view_height);
    //             self.feed_pane.should_render = true;
    //         }
    //
    //         self.tweet_pane.should_render = true;
    //         self.bottom_bar.should_render = true;
    //     }
    //
    //     self.render().await
    // }

    pub async fn render(&mut self) -> Result<()> {
        self.set_mode(Mode::Interactive)?;

        if self.feed_pane.should_render {
            self.feed_pane.component.render(
                &mut self.layout.stdout,
                0, 0, self.layout.screen_cols, self.layout.screen_rows
            )?;
            self.feed_pane.should_render = false;
        }

        {
            // let tweets = self.tweets.lock().await;
            // let tweets_reverse_chronological = self.tweets_reverse_chronological.lock().await;
            //
            // if self.tweet_pane.should_render {
            //     self.tweet_pane.component.render(
            //         &self.layout,
            //         &tweets[&tweets_reverse_chronological[self.tweets_selected_index]],
            //     )?;
            //     self.tweet_pane.should_render = false;
            // }
            //
            // if self.bottom_bar.should_render {
            //     self.bottom_bar.component.render(
            //         &self.layout,
            //         &tweets_reverse_chronological,
            //         self.tweets_selected_index,
            //     )?;
            //     self.bottom_bar.should_render = false;
            // }
        }

        let mut stdout = &self.layout.stdout;
        let focus = self.feed_pane.component.get_cursor();
        queue!(&self.layout.stdout, cursor::MoveTo(focus.0, focus.1))?;
        //     cursor::MoveTo(
        //         16,
        //         (self.tweets_selected_index - self.tweets_view_offset) as u16
        //     )
        // )?;
        stdout.flush()?;
        Ok(())
    }

    pub async fn log_selected_tweet(&mut self) -> Result<()> {
        {
            let tweets = self.tweets.lock().unwrap();
            let tweets_reverse_chronological = self.tweets_reverse_chronological.lock().unwrap();
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
        println!("{message}\r");
        Ok(())
    }

    // CR: need to sift results
    // CR: need a fixed page size, then call the twitterclient as many times as needed to achieve
    // the desired page effect
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
                    let mut tweets = tweets.lock().unwrap();
                    for tweet in new_tweets {
                        new_tweets_reverse_chronological.push(tweet.id.clone());
                        tweets.insert(tweet.id.clone(), tweet);
                    }
                }
                {
                    let mut tweets_reverse_chronological =
                        tweets_reverse_chronological.lock().unwrap();
                    tweets_reverse_chronological.append(&mut new_tweets_reverse_chronological);
                }
                Ok(())
            }
            .await
            {
                Ok(()) => event_sender.send(InternalEvent::FeedUpdated),
                Err(error) => event_sender.send(InternalEvent::LogError(error)),
            }
        });
    }

    async fn handle_internal_event(&mut self, event: InternalEvent) -> Result<()> {
        match event {
            InternalEvent::FeedUpdated => {
                self.feed_pane.should_render = true;
                self.render().await?;
            }
            InternalEvent::LogError(err) => {
                self.log_message(err.to_string().as_str())?;
            }
        }
        Ok(())
    }

    async fn handle_terminal_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(key_event) => match key_event.code {
                KeyCode::Esc => {
                    self.feed_pane.should_render = true;
                    // self.tweet_pane.should_render = true;
                    // self.bottom_bar.should_render = true;
                    self.render().await?
                }
                // KeyCode::Up => self.move_selected_index(-1).await?,
                // KeyCode::Down => self.move_selected_index(1).await?,
                KeyCode::Char('h') => self.log_message("hello")?,
                KeyCode::Char('i') => self.log_selected_tweet().await?,
                KeyCode::Char('n') => {
                    self.do_load_page_of_tweets(false);
                }
                KeyCode::Char('q') => {
                    reset();
                    process::exit(0);
                }
                _ => {
                    self.feed_pane.component.handle_key_event(key_event);
                },
            },
            Event::Resize(cols, rows) => self.resize(cols, rows),
            _ => (),
        }
        Ok(())
    }

    pub async fn event_loop(&mut self) -> Result<()> {
        let mut terminal_event_stream = EventStream::new();

        loop {
            let terminal_event = terminal_event_stream.next().fuse();
            let internal_event = self.events.1.recv();

            tokio::select! {
                event = terminal_event => {
                    if let Some(Ok(event)) = event {
                        self.handle_terminal_event(event).await?;
                    }
                },
                event = internal_event => {
                    if let Some(event) = event {
                        self.handle_internal_event(event).await?;
                    }
                }
            }
        }
    }
}

pub fn reset() {
    execute!(stdout(), LeaveAlternateScreen).unwrap();
    terminal::disable_raw_mode().unwrap()
}
