use crate::{Error, ErrorCode, Result};

pub fn validate_one_line(command: &str) -> Result<()> {
    if command.is_empty()
        || command.contains(['\r', '\n', '\0'])
        || command.chars().any(char::is_control)
    {
        return Err(Error::new(
            ErrorCode::CommandDenied,
            "policy",
            "command must be one nonempty line without control characters",
        ));
    }
    Ok(())
}

pub fn authorize(command: &str, allowed: &[String]) -> Result<()> {
    validate_one_line(command)?;
    let verb = command.split_ascii_whitespace().next().unwrap_or_default();
    if allowed.iter().any(|rule| command == rule || verb == rule) {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::CommandDenied,
        "policy",
        format!("command verb {verb:?} is not allowed by the board profile"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_shell_operators_on_one_line() {
        validate_one_line("printf hello | wc -c").unwrap();
    }
    #[test]
    fn rejects_embedded_newlines() {
        assert_eq!(
            validate_one_line("uname\nid").unwrap_err().code,
            ErrorCode::CommandDenied
        );
    }
    #[test]
    fn allows_configured_verb() {
        authorize("uname -a", &["uname".into()]).unwrap();
    }
}
