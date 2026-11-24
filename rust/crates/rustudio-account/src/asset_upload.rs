use serde_json::Value;
use std::time::Duration;

pub(crate) const ASSETS_PATH: &str = "/assets/user-auth/v1/assets";
pub(crate) const OPERATIONS_PATH: &str = "/assets/user-auth/v1/operations";

pub(crate) fn multipart(
    request: &Value,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> (String, Vec<u8>) {
    let mut boundary = "----rustudio-asset-upload-7c1f".to_owned();
    while bytes
        .windows(boundary.len())
        .any(|part| part == boundary.as_bytes())
    {
        boundary.push('x');
    }
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"request\"\r\nContent-Type: application/json\r\n\r\n{request}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"fileContent\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

pub(crate) fn wait_for_operation<E>(
    initial: &str,
    operations_url: &str,
    mut fetch: impl FnMut(&str) -> Result<String, E>,
    error: impl Fn(String) -> E,
) -> Result<Value, E> {
    let parse = |body: &str| {
        serde_json::from_str::<Value>(body)
            .map_err(|e| error(format!("Invalid upload operation reply: {e}")))
    };
    let mut operation = parse(initial)?;
    let mut poll_url = None;
    for attempt in 0..=60 {
        if let Some(failure) = operation.get("error").filter(|v| !v.is_null()) {
            return Err(error(format!("Upload operation failed: {failure}")));
        }
        if operation.get("done").and_then(Value::as_bool) == Some(true) {
            return Ok(operation);
        }
        if attempt == 60 {
            break;
        }
        if poll_url.is_none() {
            let path = operation
                .get("path")
                .or_else(|| operation.get("operationId"))
                .and_then(Value::as_str)
                .ok_or_else(|| error("Upload reply is missing its operation ID".into()))?;
            let absolute_prefix = format!("{operations_url}/");
            let route_prefix = format!("{OPERATIONS_PATH}/");
            let id = path
                .strip_prefix("operations/")
                .or_else(|| path.strip_prefix(&route_prefix))
                .or_else(|| path.strip_prefix(&absolute_prefix))
                .unwrap_or(path);
            // Only a single opaque ID can be appended to the configured endpoint.
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(error(
                    "Upload reply contains an invalid operation path".into(),
                ));
            }
            poll_url = Some(format!("{operations_url}/{id}"));
        }
        std::thread::sleep(Duration::from_secs(1));
        operation = parse(&fetch(poll_url.as_deref().unwrap())?)?;
    }
    Err(error(
        "Upload operation did not finish after 60 polls".into(),
    ))
}

/// Studio sends uploads above `FIntAssetsUploadUseMultiPartSizeThresholdBytes`
/// through the chunked route, in `DFIntMultiPartUploadMaxChunkSizeByte`
/// pieces (both 7 MiB in PCStudioApp's settings). The single-part form
/// endpoint refuses bodies over 50 MiB.
pub(crate) const MULTIPART_THRESHOLD: usize = 7_340_032;
const MULTIPART_CHUNK: usize = 7_340_032;
/// `FIntPublicStudioPlaceUploadSizeLimitBytes` (98 MiB).
pub(crate) const PLACE_SIZE_LIMIT: usize = 102_760_448;
/// `DFIntMultiPartUploadS3RequestTimeout`.
const S3_TIMEOUT: Duration = Duration::from_secs(120);

/// Chunked upload, the way Studio does it:
/// 1. PATCH `…/assets/{id}:multipartUpload` (or POST `…/assets:multipartUpload`
///    for a new asset) with `{asset, file: {contentType, filesize,
///    md5Checksum (hex), chunkPlan: [sizes]}}` → `{operationPath,
///    uploadUrls: [{httpVerb, url, chunkNum, contentStart, contentLength}]}`;
/// 2. each chunk goes to its presigned S3 URL (no cookie), then
///    POST `…/{operationPath}:multipartUploadChunkComplete` `{chunkNum, etag}`
///    with the ETag S3 answered;
/// 3. POST `…/{operationPath}:multipartUploadComplete` `{}` returns the
///    operation, polled like a single-part upload.
#[allow(clippy::too_many_arguments)]
pub(crate) fn multipart_upload<E>(
    session: &crate::session::Session,
    apis: &str,
    method: &str,
    start_url: &str,
    asset: &Value,
    content_type: &str,
    bytes: &[u8],
    error: impl Fn(String) -> E,
    check_reply: impl Fn(u16, String) -> Result<String, E>,
) -> Result<String, E> {
    use md5::{Digest, Md5};
    let md5: String = Md5::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    let plan: Vec<usize> = bytes.chunks(MULTIPART_CHUNK).map(<[u8]>::len).collect();
    let start = serde_json::json!({
        "asset": asset,
        "file": {
            "contentType": content_type,
            "filesize": bytes.len(),
            "md5Checksum": md5,
            "chunkPlan": plan,
        },
    });
    let call = |method: &str, url: &str, body: &[u8]| -> Result<String, E> {
        let (status, text) = session
            .send_csrf_with_timeout(method, url, "application/json", body, Some(Duration::from_secs(60)))
            .map_err(|e| error(e.0))?;
        check_reply(status, text)
    };
    let reply = call(method, start_url, start.to_string().as_bytes())?;
    let reply: Value = serde_json::from_str(&reply)
        .map_err(|e| error(format!("Invalid multipart upload reply: {e}")))?;
    let operation = reply
        .get("operationPath")
        .and_then(Value::as_str)
        .and_then(|p| p.strip_prefix("operations/"))
        .filter(|id| {
            !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .ok_or_else(|| error("Multipart upload reply has no valid operationPath".into()))?;
    let base = format!("{apis}{OPERATIONS_PATH}/{operation}");
    let urls = reply
        .get("uploadUrls")
        .and_then(Value::as_array)
        .filter(|urls| !urls.is_empty())
        .ok_or_else(|| error("Multipart upload reply has no uploadUrls".into()))?;
    let number = |v: Option<&Value>| -> Option<usize> {
        v.and_then(|v| v.as_u64().map(|n| n as usize).or_else(|| v.as_str()?.parse().ok()))
    };
    let s3: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(S3_TIMEOUT))
        .build()
        .into();
    for part in urls {
        let chunk = number(part.get("chunkNum"))
            .ok_or_else(|| error("Multipart upload URL has no chunkNum".into()))?;
        let start = number(part.get("contentStart")).unwrap_or(0);
        let len = number(part.get("contentLength"))
            .ok_or_else(|| error(format!("Multipart chunk {chunk} has no contentLength")))?;
        let piece = start
            .checked_add(len)
            .and_then(|end| bytes.get(start..end))
            .ok_or_else(|| error(format!("Multipart chunk {chunk} is outside the file")))?;
        let url = part
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| u.starts_with("https://"))
            .ok_or_else(|| error(format!("Multipart chunk {chunk} has no https upload URL")))?;
        let verb = part.get("httpVerb").and_then(Value::as_str).unwrap_or("PUT");
        if verb != "PUT" {
            return Err(error(format!("Multipart chunk {chunk} wants HTTP {verb}")));
        }
        let mut sent = None;
        for _ in 0..3 {
            match s3.put(url).send(piece) {
                Ok(r) if r.status().is_success() => {
                    sent = r.headers().get("etag").and_then(|h| h.to_str().ok()).map(str::to_owned);
                    if sent.is_some() {
                        break;
                    }
                }
                _ => std::thread::sleep(Duration::from_secs(1)),
            }
        }
        let etag = sent.ok_or_else(|| error(format!("Uploading chunk {chunk} to storage failed")))?;
        let done = serde_json::json!({ "chunkNum": chunk, "etag": etag });
        call("POST", &format!("{base}:multipartUploadChunkComplete"), done.to_string().as_bytes())?;
    }
    call("POST", &format!("{base}:multipartUploadComplete"), b"{}")
}
