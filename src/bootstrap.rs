/// Selects how a fresh Agent is initialized.
///
/// This remains separate from session resume because archived sessions already
/// contain the system messages they were created with. Future variants may
/// also select different tool sets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Bootstrap {
    #[default]
    Default,
    Instruction(String),
}

impl From<Option<String>> for Bootstrap {
    fn from(instruction: Option<String>) -> Self {
        match instruction {
            Some(instruction) => Self::Instruction(instruction),
            None => Self::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_instruction_selects_the_default_bootstrap() {
        assert_eq!(Bootstrap::from(None), Bootstrap::Default);
    }

    #[test]
    fn instruction_selects_an_instruction_bootstrap() {
        assert_eq!(
            Bootstrap::from(Some("You are a focused reviewer.".to_owned())),
            Bootstrap::Instruction("You are a focused reviewer.".to_owned())
        );
    }
}
