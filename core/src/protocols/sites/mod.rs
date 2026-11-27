//! Site resolvers: turn a known page URL into direct media URLs.
pub(crate) mod x;
pub(crate) mod youtube;

/// A request error without its URL (site query strings stay out of messages), ending
/// in words `util::error_code` classifies as network for connect failures and timeouts.
pub(crate) fn request_error(error: reqwest::Error) -> String {
    let (connect, timeout) = (error.is_connect(), error.is_timeout());
    let text = error.without_url().to_string();
    if timeout {
        format!("{text}: timed out")
    } else if connect {
        format!("{text}: connection failed")
    } else {
        text
    }
}
