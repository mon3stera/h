use std::{fmt::Write, str::FromStr};

use async_openai::types::responses::{
    EasyInputMessage, FunctionToolCall, InputItem, InputParam, Item, ItemReference,
    ItemReferenceType, MessageItem, OutputItem, OutputMessage, Role,
};

pub struct Context<M> {
    buf: String,
    histories: Vec<M>,
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
