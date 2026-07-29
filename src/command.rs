/// A command entered through the prompt box instead of sent to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Clear,
    Compact,
}

impl Command {
    pub const ALL: [Self; 2] = [Self::Clear, Self::Compact];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Clear => "/clear",
            Self::Compact => "/compact",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Clear => "start a new session",
            Self::Compact => "compact this session",
        }
    }

    pub fn matching(input: &str) -> Vec<Self> {
        if !input.starts_with('/') || input.chars().any(char::is_whitespace) {
            return Vec::new();
        }

        let prefix = input.to_ascii_lowercase();

        Self::ALL
            .into_iter()
            .filter(|command| command.label().starts_with(&prefix))
            .collect()
    }

    pub fn parse(input: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|command| command.label().eq_ignore_ascii_case(input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slash_lists_every_command() {
        assert_eq!(Command::matching("/"), Command::ALL);
    }

    #[test]
    fn matching_uses_a_case_insensitive_prefix() {
        assert_eq!(Command::matching("/CO"), [Command::Compact]);
    }

    #[test]
    fn ordinary_and_multiline_input_have_no_matches() {
        assert!(Command::matching("compact").is_empty());
        assert!(Command::matching("/compact now").is_empty());
        assert!(Command::matching("/compact\nmore").is_empty());
    }

    #[test]
    fn parsing_requires_a_complete_command() {
        assert_eq!(Command::parse("/CLEAR"), Some(Command::Clear));
        assert_eq!(Command::parse("/cl"), None);
    }
}
