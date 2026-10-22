//! 截图 / 端到端脚本用的本地模拟 GitHub Gist 接口，监听 127.0.0.1 的随机端口，
//! 打印 `READY <端口>` 之后一直跑到被结束。数据只在内存里，不碰真实的 GitHub。
//!
//! 令牌是 `demo-token-not-a-real-github-token`（经典令牌，只有 `gist` 权限），用户是 `demo-user`。
//! 新建的 Gist ID 形如 `mock0000…0001`，不是 32 位十六进制，截图里不会和真实的 Gist ID 混淆。

#[path = "../tests/common/mock_github.rs"]
mod mock_github;

use mock_github::{GithubOptions, MockGithub};

#[tokio::main]
async fn main() {
    let github = MockGithub::start(GithubOptions::classic(
        "demo-token-not-a-real-github-token",
        "demo-user",
    ))
    .await;
    println!("READY {}", github.addr().port());
    // 一直跑到被结束（脚本只结束自己启动的这个进程）。
    std::future::pending::<()>().await;
}
