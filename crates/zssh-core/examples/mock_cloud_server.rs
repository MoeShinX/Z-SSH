//! 截图 / 端到端脚本用的本地模拟存储服务（WebDAV 或 S3），监听 127.0.0.1 的随机端口，
//! 打印 `READY <端口>` 之后一直跑到被结束。数据只在内存里，不碰任何真实云账号。
//!
//! 用法：`mock_cloud_server webdav` 或 `mock_cloud_server s3`。
//! WebDAV 用户名 / 密码：`alice` / `dav-demo-password`；
//! S3：bucket `zssh-demo`，Access Key `AKIADEMOKEY`，Secret `s3-demo-secret-key`，路径风格。

#[path = "../tests/common/mock_cloud.rs"]
mod mock_cloud;

use mock_cloud::{Behavior, Kind, MockCloud, S3Conditional};

#[tokio::main]
async fn main() {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "webdav".to_owned());
    let kind = match mode.as_str() {
        "webdav" => Kind::WebDav {
            user: "alice".to_owned(),
            password: "dav-demo-password".to_owned(),
            behavior: Behavior::Full,
            put_returns_etag: true,
        },
        "s3" => Kind::S3 {
            access_key: "AKIADEMOKEY".to_owned(),
            secret_key: "s3-demo-secret-key".to_owned(),
            region: "us-east-1".to_owned(),
            bucket: "zssh-demo".to_owned(),
            path_style: true,
            conditional: S3Conditional::Full,
        },
        other => {
            eprintln!("不认识的模式：{other}（只有 webdav / s3）");
            std::process::exit(2);
        }
    };
    let cloud = MockCloud::start(kind).await;
    println!("READY {}", cloud.addr().port());
    // 一直跑到被结束（脚本只结束自己启动的这个进程）。
    std::future::pending::<()>().await;
}
