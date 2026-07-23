use std::{io, path::PathBuf};

use serde_json::Value;
use shellexpand::tilde;
use tokio::{
    fs::{self, File},
    io::AsyncReadExt,
};

#[derive(Clone, Debug)]
pub enum Message {
    System(String),
    User(String),
    Assistant(String),
    ToolCallResult {
        call_id: String,
        output: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: String,
    },
}

pub struct Context<M> {
    buf: String,
    histories: Vec<M>,
}

impl Context<Message> {
    pub async fn inject_global_prompts(&mut self) -> anyhow::Result<&mut Self> {
        let mut prompts = Vec::new();
        let mut total_bytes = 0_usize;

        for f in extra_prompt_paths() {
            let mut file = match File::open(f).await {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(anyhow::Error::from(e)),
            };

            let mut content = String::new();

            file.read_to_string(&mut content).await?;
            total_bytes += content.len();

            prompts.push(content);
        }

        tracing::info!(
            event = "context.global_prompts.loaded",
            prompt_source_count = prompts.len(),
            total_bytes
        );

        self.histories_mut()
            .push(Message::System(prompts.join("\n")));
        Ok(self)
    }
}

impl<M> Context<M> {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            histories: Vec::new(),
        }
    }

    pub fn prepare_buf(&mut self) {
        self.buf = String::new();
    }

    pub fn append_buf(&mut self, n: impl AsRef<str>) {
        self.buf.push_str(n.as_ref());
    }

    pub fn finalize_buf(&mut self, f: Box<dyn FnOnce(String) -> M>) {
        let mut buf = String::new();
        std::mem::swap(&mut buf, &mut self.buf);
        self.histories.push(f(buf));
    }

    pub fn histories(&self) -> &[M] {
        &self.histories
    }

    pub fn histories_mut(&mut self) -> &mut Vec<M> {
        &mut self.histories
    }
}

fn extra_prompt_paths() -> Vec<PathBuf> {
    vec![".h/AGENTS.md", "~/.claude/CLAUDE.md", "~/.h/AGENTS.md"]
        .into_iter()
        .map(|s| PathBuf::from(tilde(s).as_ref()))
        .collect()
}
