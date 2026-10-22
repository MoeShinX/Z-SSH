//! AWS Signature Version 4（只用于 S3 的单个对象请求）。
//!
//! 自己实现而不引入 AWS SDK：算法就是几步 HMAC-SHA256。HMAC 在 `sha2` 0.10 上手写
//! （工作区里的 `hmac` 0.13 用的是更新的 `digest`，和 `sha2` 0.10 对不上），
//! 用 RFC 4231 的向量验证；整个签名用 AWS 文档里的示例验证（见测试）。

use sha2::{Digest, Sha256};

const BLOCK: usize = 64;

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(data);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner_digest);
    outer.finalize().into()
}

/// SigV4 的 URI 编码：只有 `A-Za-z0-9-_.~` 不编码；`/` 按 `keep_slash` 决定。
pub fn uri_encode(input: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[derive(Clone)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
}

pub struct SigningInput<'a> {
    pub method: &'a str,
    /// 已经按 [`uri_encode`] 编码好的路径（以 `/` 开头）。
    pub canonical_uri: &'a str,
    /// 已经排序、编码好的查询串（没有就是空串）。
    pub canonical_query: &'a str,
    /// 要签名的头：名字小写、值去掉首尾空白。不要求有序，这里会排序。
    pub headers: &'a [(String, String)],
    pub payload_hash: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    /// `YYYYMMDDTHHMMSSZ`
    pub amz_date: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Signature {
    pub authorization: String,
    pub signed_headers: String,
    pub signature: String,
    pub canonical_request: String,
}

fn canonical_request(input: &SigningInput<'_>) -> (String, String) {
    let mut headers: Vec<(&str, String)> = input
        .headers
        .iter()
        .map(|(name, value)| {
            // 值里连续空白折成一个空格。
            let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
            (name.as_str(), value)
        })
        .collect();
    headers.sort_by(|a, b| a.0.cmp(b.0));
    let canonical_headers: String = headers
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();
    let signed_headers = headers
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(";");
    let request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        input.method,
        input.canonical_uri,
        input.canonical_query,
        canonical_headers,
        signed_headers,
        input.payload_hash
    );
    (request, signed_headers)
}

pub fn sign(credentials: &Credentials, input: &SigningInput<'_>) -> Signature {
    let (canonical_request, signed_headers) = canonical_request(input);
    let date = &input.amz_date[..8];
    let scope = format!("{date}/{}/{}/aws4_request", input.region, input.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        input.amz_date,
        sha256_hex(canonical_request.as_bytes())
    );
    let k_date = hmac_sha256(
        format!("AWS4{}", credentials.secret_key).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, input.region.as_bytes());
    let k_service = hmac_sha256(&k_region, input.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope},SignedHeaders={signed_headers},Signature={signature}",
        credentials.access_key
    );
    Signature {
        authorization,
        signed_headers,
        signature,
        canonical_request,
    }
}

/// Unix 秒 → `YYYYMMDDTHHMMSSZ`（UTC）。
pub fn amz_date(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    // Howard Hinnant 的 civil_from_days。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests;
