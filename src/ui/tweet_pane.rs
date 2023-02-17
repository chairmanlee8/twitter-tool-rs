use crate::twitter_client::api;
use crate::ui::{BoundingBox, Input, Render};
use anyhow::Result;
use crossterm::event::KeyEvent;
use crossterm::terminal::{self, ClearType};
use crossterm::{cursor, queue, style};
use std::collections::HashMap;
use std::io::Stdout;
use std::sync::{Arc, Mutex};

pub struct TweetPane {
    tweets: Arc<Mutex<HashMap<String, api::Tweet>>>,
    selected_tweet_id: Option<String>,
}

impl TweetPane {
    pub fn new(tweets: &Arc<Mutex<HashMap<String, api::Tweet>>>) -> Self {
        Self {
            tweets: tweets.clone(),
            selected_tweet_id: None,
        }
    }

    pub fn set_selected_tweet_id(&mut self, tweet_id: Option<String>) {
        self.selected_tweet_id = tweet_id;
    }
}

impl Render for TweetPane {
    fn render(&mut self, stdout: &mut Stdout, bounding_box: BoundingBox) -> Result<()> {
        let BoundingBox {
            left,
            top,
            width,
            height,
        } = bounding_box;

        if let Some(tweet_id) = &self.selected_tweet_id {
            let str_unknown = String::from("[unknown]");

            let tweets = self.tweets.lock().unwrap();
            let tweet = &tweets[tweet_id];
            let tweet_time = tweet.created_at.format("%Y-%m-%d %H:%M:%S");
            let tweet_author_username = tweet.author_username.as_ref().unwrap_or(&str_unknown);
            let tweet_author_name = tweet.author_name.as_ref().unwrap_or(&str_unknown);
            let tweet_author = format!("@{tweet_author_username} [{tweet_author_name}]");
            let tweet_lines = textwrap::wrap(&tweet.text, width.saturating_sub(1) as usize);

            let mut row = top;

            queue!(stdout, cursor::MoveTo(left, row))?;
            queue!(stdout, terminal::Clear(ClearType::UntilNewLine))?;
            queue!(stdout, style::Print(&tweet_time))?;
            row += 1;

            queue!(stdout, cursor::MoveTo(left, row))?;
            queue!(stdout, terminal::Clear(ClearType::UntilNewLine))?;
            queue!(stdout, style::Print(&tweet_author))?;
            row += 2;

            for tweet_line in tweet_lines {
                queue!(stdout, cursor::MoveTo(left, row))?;
                queue!(stdout, terminal::Clear(ClearType::UntilNewLine))?;
                queue!(stdout, style::Print(&tweet_line))?;
                row += 1;
            }

            while row < top + height {
                queue!(stdout, cursor::MoveTo(left, row))?;
                queue!(stdout, terminal::Clear(ClearType::UntilNewLine))?;
                row += 1;
            }
        }

        Ok(())
    }
}

impl Input for TweetPane {
    fn handle_key_event(&mut self, _event: KeyEvent) {
        todo!()
    }

    fn get_cursor(&self, bounding_box: BoundingBox) -> (u16, u16) {
        (bounding_box.left, bounding_box.top)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_segmentation() {
        // NB: expectation is correct; reasoning may be subtle
        let str =
            "Why did the chicken cross the road?\n\nBecause he wanted to get to the other side.";
        let result = textwrap::wrap(str, 20);
        assert_eq!(
            result,
            vec![
                "Why did the chicken",
                "cross the road?",
                "",
                "Because he wanted",
                "to get to the other",
                "side."
            ]
        );
    }
}
