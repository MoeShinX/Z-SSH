//! 一键下载 opencode：查官方 GitHub Release、下载 Windows x64 zip、解压出
//! `opencode.exe`。代理走 Z-SSH 的默认代理设置（拼好 URL 传进来）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

/// 官方 Release API（GitHub 会把 sst/opencode 重定向到这里）。
pub const RELEASE_API: &str = "https://api.github.com/repos/anomalyco/opencode/releases/latest";
/// 要下载的资产名（Windows x64 普通版；baseline 和 arm64 不选）。
pub const WINDOWS_X64_ASSET: &str = "opencode-windows-x64.zip";
/// 下载地址必须在 GitHub 上（Release 资产的直链都是这个前缀，之后的重定向由 GitHub 发起）。
const DOWNLOAD_URL_PREFIX: &str = "https://github.com/";
/// 资产大小上限：远大于真实包（几十 MiB），只防一个异常的响应把磁盘写满。
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
/// 解出来的 opencode.exe 最大多少字节：压缩包大小管不住解压后的大小。
const MAX_EXTRACTED_BYTES: u64 = 1024 * 1024 * 1024;
/// 最多跟几次跳转（GitHub 的下载地址会跳到 objects / release-assets 子域名）。
const MAX_REDIRECTS: usize = 10;

/// 最新 Release 里我们关心的信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseInfo {
    /// 版本号（tag，如 `v1.19.0`）。
    pub version: String,
    /// 资产名。
    pub asset_name: String,
    /// 字节数。
    pub size: u64,
    /// GitHub 给这个资产算的 SHA-256（小写十六进制）。下载完必须一致才解压、才运行（R-10 M8）。
    pub sha256: String,
    /// 资产直链。
    pub download_url: String,
    /// Release 页面（展示给用户看的来源）。
    pub page_url: String,
}

/// 下载出错（含用户取消）。
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("用户取消")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
}

/// 组装 HTTP 客户端。`proxy_url` 形如 `socks5://127.0.0.1:1080` 或
/// `http://user:pass@127.0.0.1:8080`，None 表示直连。
pub fn build_client(proxy_url: Option<&str>) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        // GitHub API 拒绝没有 User-Agent 的请求。
        .user_agent("Z-SSH")
        .connect_timeout(std::time::Duration::from_secs(20))
        // 跳转也只能去 GitHub 自己的 https 地址：初始地址检查管不到中途跳去哪。
        // 下载完还要核对摘要，这一层是让请求根本不去别处。
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("跳转次数太多")
            } else if redirect_allowed(attempt.url()) {
                attempt.follow()
            } else {
                let target = attempt.url().to_string();
                attempt.error(format!("下载被跳转到了 GitHub 以外的地址：{target}"))
            }
        }));
    if let Some(url) = proxy_url {
        let proxy = reqwest::Proxy::all(url).map_err(|e| format!("代理地址无效：{e}"))?;
        builder = builder.proxy(proxy);
    } else {
        // 没有代理 = 直连：用户选了“不使用代理”时，不能被系统代理或 HTTP_PROXY 环境变量带走。
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败：{e}"))
}

/// 跳转目标是否可信：只接受 https 的 github.com / *.github.com / *.githubusercontent.com。
fn redirect_allowed(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    ["github.com", "githubusercontent.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// 查最新 Release 里的 Windows x64 资产。
pub async fn latest_release(client: &reqwest::Client) -> Result<ReleaseInfo, String> {
    let resp = client
        .get(RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("访问 GitHub 失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub 返回 {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析 Release 信息失败：{e}"))?;
    parse_release(&body)
}

/// 从 Release JSON 里挑出 Windows x64 资产。
pub fn parse_release(body: &serde_json::Value) -> Result<ReleaseInfo, String> {
    let version = body["tag_name"]
        .as_str()
        .ok_or("Release 里没有 tag_name")?
        .to_string();
    let page_url = body["html_url"].as_str().unwrap_or("").to_string();
    let assets = body["assets"].as_array().ok_or("Release 里没有 assets")?;
    let asset = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(WINDOWS_X64_ASSET))
        .ok_or_else(|| format!("最新 Release 里没有 {WINDOWS_X64_ASSET}"))?;
    // 下载的东西随后会被直接运行：没有能核对的摘要就不下（R-10 M8）。TLS 之外只有它能证明
    // 下下来的就是 Release 里那一份（代理 / 中间盒 / CDN 一侧被改过都能发现）。
    let sha256 = asset["digest"]
        .as_str()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .map(str::to_ascii_lowercase)
        .filter(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| {
            "这个 Release 没有提供 SHA-256 摘要，无法核对下载内容，已停止。可以到发布页手动下载安装"
                .to_string()
        })?;
    let size = asset["size"].as_u64().unwrap_or(0);
    if size == 0 || size > MAX_DOWNLOAD_BYTES {
        return Err(format!("Release 里的文件大小不正常（{size} 字节），已停止"));
    }
    let download_url = asset["browser_download_url"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if !download_url.starts_with(DOWNLOAD_URL_PREFIX) {
        return Err(format!("下载地址不在 GitHub 上（{download_url}），已停止"));
    }
    Ok(ReleaseInfo {
        version,
        asset_name: WINDOWS_X64_ASSET.to_string(),
        size,
        sha256,
        download_url,
        page_url,
    })
}

/// 核对下载下来的文件：大小和 SHA-256 都要和 Release 公布的一致。不一致时**删掉文件**，
/// 调用方不能再解压或运行它。
pub fn verify_download(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};

    let check = || -> Result<(), String> {
        let mut file = std::fs::File::open(path).map_err(|e| format!("读下载文件失败：{e}"))?;
        let mut hasher = Sha256::new();
        let size =
            std::io::copy(&mut file, &mut hasher).map_err(|e| format!("读下载文件失败：{e}"))?;
        if size != expected_size {
            return Err(format!(
                "下载的文件大小（{size} 字节）和 Release 公布的（{expected_size} 字节）不一致"
            ));
        }
        let actual: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if !actual.eq_ignore_ascii_case(expected_sha256) {
            return Err(
                "下载的文件和 Release 公布的 SHA-256 不一致（可能被代理或中间人改过）".to_string(),
            );
        }
        Ok(())
    };
    check().map_err(|reason| {
        let _ = std::fs::remove_file(path);
        format!("{reason}，已删除下载的文件，没有安装")
    })
}

/// 下载到本地文件；每次回调 `(已下载, 总量)`，1MiB 节流；`cancel` 置位就中断。
///
/// `max_bytes`：最多收这么多字节（Release 公布的大小），多了就中断——下载完还要
/// [`verify_download`]，这里只防异常的响应把磁盘写满。
pub async fn download_to(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    max_bytes: u64,
    mut on_progress: impl FnMut(u64, u64) + Send,
    cancel: &AtomicBool,
) -> Result<(), DownloadError> {
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| DownloadError::Failed(format!("下载失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(DownloadError::Failed(format!("下载返回 {}", resp.status())));
    }
    let total = resp.content_length().unwrap_or(0);
    if total > max_bytes {
        return Err(DownloadError::Failed(format!(
            "服务器给的大小（{total} 字节）超过了 Release 公布的大小（{max_bytes} 字节），没有下载"
        )));
    }

    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| DownloadError::Failed(format!("创建下载文件失败：{e}")))?;
    let mut received: u64 = 0;
    let mut last_reported: u64 = 0;
    on_progress(0, total);

    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| DownloadError::Failed(format!("下载中断：{e}")))?
    {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = tokio::fs::remove_file(dest).await;
            return Err(DownloadError::Cancelled);
        }
        // 先算再写：超出的那一块不落盘，磁盘占用严格不超过 max_bytes。
        received += chunk.len() as u64;
        if received > max_bytes {
            drop(file);
            let _ = tokio::fs::remove_file(dest).await;
            return Err(DownloadError::Failed(format!(
                "下载的内容超过了 Release 公布的大小（{max_bytes} 字节），已停止"
            )));
        }
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| DownloadError::Failed(format!("写文件失败：{e}")))?;
        // 1MiB 节流，别把进度事件刷爆。
        if received - last_reported >= 1024 * 1024 {
            last_reported = received;
            on_progress(received, total);
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|e| DownloadError::Failed(format!("刷新下载文件失败：{e}")))?;
    drop(file);
    on_progress(received, total);

    if total > 0 && received != total {
        return Err(DownloadError::Failed(format!(
            "下载不完整：{received}/{total} 字节"
        )));
    }
    Ok(())
}

/// 从 zip 里解出 `opencode.exe` 到 `out_dir`（平铺），返回 exe 路径。
///
/// zip 没有 exe 或者解压失败都报错，调用方负责把半成品目录清掉。
pub fn extract_exe(zip_path: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    extract_exe_limited(zip_path, out_dir, MAX_EXTRACTED_BYTES)
}

/// 同 [`extract_exe`]，解出来的文件超过 `limit` 字节就删掉并报错。
fn extract_exe_limited(zip_path: &Path, out_dir: &Path, limit: u64) -> Result<PathBuf, String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("打开压缩包失败：{e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("压缩包损坏或格式不对：{e}"))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取压缩包条目失败：{e}"))?;
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let is_exe = name
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("opencode.exe"))
            .unwrap_or(false);
        if !is_exe {
            continue;
        }
        std::fs::create_dir_all(out_dir).map_err(|e| format!("创建目录失败：{e}"))?;
        let dest = out_dir.join("opencode.exe");
        if entry.size() > limit {
            return Err(format!(
                "压缩包里的 opencode.exe 声称有 {} 字节，超过上限 {limit} 字节，没有解压",
                entry.size()
            ));
        }
        let mut out = std::fs::File::create(&dest).map_err(|e| format!("写 {dest:?} 失败：{e}"))?;
        // 条目头里的大小可以是假的：实际解压时也只读到上限多一个字节为止。
        let written = std::io::copy(&mut std::io::Read::take(&mut entry, limit + 1), &mut out)
            .map_err(|e| format!("解压失败：{e}"))?;
        if written > limit {
            drop(out);
            let _ = std::fs::remove_file(&dest);
            return Err(format!(
                "解出来的 opencode.exe 超过上限 {limit} 字节，已删除"
            ));
        }
        return Ok(dest);
    }
    Err(format!(
        "压缩包里没有 opencode.exe（{}）",
        zip_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_and_picks_x64_asset() {
        let body = serde_json::json!({
            "tag_name": "v1.19.0",
            "html_url": "https://github.com/anomalyco/opencode/releases/tag/v1.19.0",
            "assets": [
                {"name": "opencode-windows-x64-baseline.zip", "size": 62173954,
                 "browser_download_url": "https://example.invalid/baseline.zip"},
                {"name": "opencode-windows-arm64.zip", "size": 55000000,
                 "browser_download_url": "https://example.invalid/arm64.zip"},
                {"name": "opencode-windows-x64.zip", "size": 62173956,
                 "digest": format!("sha256:{}", "AB".repeat(32)),
                 "browser_download_url": "https://github.com/anomalyco/opencode/releases/download/v1.19.0/opencode-windows-x64.zip"}
            ]
        });
        let info = parse_release(&body).unwrap();
        assert_eq!(info.version, "v1.19.0");
        assert_eq!(info.asset_name, "opencode-windows-x64.zip");
        // 不能挑成 baseline。
        assert!(info.download_url.ends_with("x64.zip"));
        assert_eq!(info.size, 62173956);
        assert_eq!(info.sha256, "ab".repeat(32), "摘要统一成小写");
    }

    fn release_with(asset: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"tag_name": "v1.19.0", "assets": [asset]})
    }

    #[test]
    fn a_release_without_a_usable_digest_or_github_url_is_refused() {
        // R-10 M8：没有摘要就没法核对下载内容，不能下了直接运行。
        let url =
            "https://github.com/anomalyco/opencode/releases/download/v1/opencode-windows-x64.zip";
        let no_digest = release_with(serde_json::json!({
            "name": WINDOWS_X64_ASSET, "size": 10, "browser_download_url": url
        }));
        assert!(parse_release(&no_digest).unwrap_err().contains("SHA-256"));
        let bad_digest = release_with(serde_json::json!({
            "name": WINDOWS_X64_ASSET, "size": 10, "digest": "sha1:abcd", "browser_download_url": url
        }));
        assert!(parse_release(&bad_digest).is_err());
        let elsewhere = release_with(serde_json::json!({
            "name": WINDOWS_X64_ASSET, "size": 10, "digest": format!("sha256:{}", "0".repeat(64)),
            "browser_download_url": "http://example.invalid/opencode-windows-x64.zip"
        }));
        assert!(parse_release(&elsewhere).unwrap_err().contains("GitHub"));
        let huge = release_with(serde_json::json!({
            "name": WINDOWS_X64_ASSET, "size": MAX_DOWNLOAD_BYTES + 1,
            "digest": format!("sha256:{}", "0".repeat(64)), "browser_download_url": url
        }));
        assert!(parse_release(&huge).is_err());
    }

    #[test]
    fn a_download_that_does_not_match_the_digest_is_deleted() {
        use sha2::{Digest, Sha256};

        let tmp = crate::test_util::TestDir::new("download-verify");
        let path = tmp.join("opencode-download.zip");
        let content = b"pretend this is the release zip";
        let good: String = Sha256::digest(content)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();

        std::fs::write(&path, content).unwrap();
        verify_download(&path, content.len() as u64, &good).expect("一致就通过");
        assert!(path.exists());

        // 内容被改过：摘要不一致，文件被删掉。
        let tampered = b"tampered content";
        std::fs::write(&path, tampered).unwrap();
        let err = verify_download(&path, tampered.len() as u64, &good).unwrap_err();
        assert!(err.contains("SHA-256"), "{err}");
        assert!(!path.exists(), "不一致的文件要删掉，不能留给后面解压运行");

        // 大小不对也一样。
        std::fs::write(&path, content).unwrap();
        assert!(verify_download(&path, 1, &good).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn parse_release_rejects_missing_assets() {
        let body = serde_json::json!({"tag_name": "v1.0.0", "assets": []});
        let err = parse_release(&body).unwrap_err();
        assert!(err.contains("opencode-windows-x64.zip"));
    }

    /// 伪造一个 zip（里面是随便什么内容的 opencode.exe）验证解压逻辑，不连网。
    #[test]
    fn extracts_exe_from_local_zip() {
        let tmp = crate::test_util::TestDir::new("download-test");
        let zip_path = tmp.join("fake.zip");

        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            // 真实包里 exe 在根目录，前面可能还有别的文件。
            writer.start_file("README.md", options).unwrap();
            std::io::Write::write_all(&mut writer, b"docs").unwrap();
            writer.start_file("opencode.exe", options).unwrap();
            std::io::Write::write_all(&mut writer, b"FAKE-EXE-CONTENT").unwrap();
            writer.finish().unwrap();
        }

        let out_dir = tmp.join("out");
        let exe = extract_exe(&zip_path, &out_dir).unwrap();
        assert_eq!(exe, out_dir.join("opencode.exe"));
        let bytes = std::fs::read(&exe).unwrap();
        assert_eq!(bytes, b"FAKE-EXE-CONTENT");
    }

    #[test]
    fn an_exe_larger_than_the_limit_is_not_left_on_disk() {
        // R7 K02：压缩包大小有上限，但解压后的大小要另外管。
        let tmp = crate::test_util::TestDir::new("download-limit");
        let zip_path = tmp.join("big.zip");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file("opencode.exe", options).unwrap();
            std::io::Write::write_all(&mut writer, &[0u8; 4096]).unwrap();
            writer.finish().unwrap();
        }
        let out_dir = tmp.join("out");
        let err = extract_exe_limited(&zip_path, &out_dir, 1000).expect_err("超过上限要拒绝");
        assert!(err.contains("上限"), "{err}");
        assert!(!out_dir.join("opencode.exe").exists(), "超限的文件不能留下");
        // 刚好在上限内照常解压。
        extract_exe_limited(&zip_path, &out_dir, 4096).expect("上限内可以解压");
    }

    #[test]
    fn redirects_only_go_to_github_over_https() {
        let ok = |url: &str| redirect_allowed(&reqwest::Url::parse(url).unwrap());
        assert!(ok(
            "https://github.com/anomalyco/opencode/releases/download/v1/x.zip"
        ));
        assert!(ok(
            "https://objects.githubusercontent.com/github-production-release-asset/x"
        ));
        assert!(ok("https://release-assets.githubusercontent.com/x"));
        assert!(ok("https://api.github.com/repos/x"));
        assert!(
            !ok("http://objects.githubusercontent.com/x"),
            "不能降成 http"
        );
        assert!(!ok("https://evil.example/x"));
        assert!(
            !ok("https://github.com.evil.example/x"),
            "后缀要按域名边界比"
        );
        assert!(!ok("https://notgithub.com/x"));
    }

    #[test]
    fn extract_reports_missing_exe() {
        let tmp = crate::test_util::TestDir::new("download-test-noexe");
        let zip_path = tmp.join("fake.zip");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file("only-readme.txt", options).unwrap();
            std::io::Write::write_all(&mut writer, b"nope").unwrap();
            writer.finish().unwrap();
        }
        let err = extract_exe(&zip_path, &tmp.join("out")).unwrap_err();
        assert!(err.contains("没有 opencode.exe"));
    }
}
