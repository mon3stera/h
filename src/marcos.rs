macro_rules! log_error {
    ($value:expr) => {
        if let Err(e) = $value {
            eprintln!("{e}")
        }
    };
}

pub(crate) use log_error;
