use crate::task::Task;

fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.origin() == b.origin()
}

pub(super) async fn request(
    client: &reqwest::Client,
    source: Option<&reqwest::Url>,
    target: &reqwest::Url,
    snapshot: &Task,
) -> Result<reqwest::Response, String> {
    let mut target = target.clone();
    for _ in 0..10 {
        let mut request = client.get(target.clone());
        if source.is_some_and(|source| same_origin(source, &target)) {
            if let Some(value) = &snapshot.authorization {
                request = request.header(reqwest::header::AUTHORIZATION, value);
            }
            if let Some(value) = &snapshot.cookie {
                request = request.header(reqwest::header::COOKIE, value);
            }
        }
        let response = request
            .send()
            .await
            .map_err(|_| "Metalink HTTP request failed")?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|h| h.to_str().ok())
                .ok_or("Metalink redirect has no Location")?;
            target = target
                .join(location)
                .map_err(|_| "invalid Metalink redirect")?;
            if !matches!(target.scheme(), "http" | "https")
                || !target.username().is_empty()
                || target.password().is_some()
            {
                return Err("invalid Metalink redirect target".into());
            }
            continue;
        }
        if !response.status().is_success()
            || response.status() == reqwest::StatusCode::PARTIAL_CONTENT
        {
            return Err(format!("Metalink HTTP {}", response.status()));
        }
        return Ok(response);
    }
    Err("too many Metalink redirects".into())
}
