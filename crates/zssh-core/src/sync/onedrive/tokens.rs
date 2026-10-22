//! 令牌的本机存储和自动刷新。
//!
//! - **存储**：访问令牌 + 刷新令牌整体用 DPAPI 封装后放在设置表的 [`TOKENS_KEY`] 里。这个键归为
//!   “只在本机”（`settings.rs` 的白名单表），所以**不进同步包、不进离线备份**；日志里也没有令牌
//!   （[`TokenSet`] 的 Debug 输出不含令牌）。
//! - **刷新**：访问令牌快过期（[`REFRESH_EARLY_SECS`] 秒内）就用刷新令牌换新的。微软每次都会换发
//!   新的刷新令牌，拿到就**先存盘再使用**，旧的丢掉。并发的请求只会有一个去刷新（进程级的锁，
//!   拿到锁后重读一遍存储，别人已经刷新过就直接用）。
//! - **刷新令牌失效**（被撤销、90 天没用过、改了密码）：返回 [`StorageError::AuthFailed`]，
//!   引擎和调度器据此暂停自动同步，界面提示重新登录。网络不通、服务暂时不可用不算失效。

use std::sync::Arc;

use reqwest::Client;
use tokio::sync::Mutex as AsyncMutex;

use super::oauth::{self, now_secs, Endpoints, OAuthError, TokenSet, REAUTH_MESSAGE};
use crate::store::Database;
use crate::sync::error::SyncError;
use crate::sync::sealer::SecretSealer;
use crate::sync::service::{clear as clear_setting, open_from_setting, seal_to_setting};
use crate::sync::storage::StorageError;

/// 令牌在设置表里的键（只在本机，`settings.rs` 里归为 LocalOnly）。
pub const TOKENS_KEY: &str = "sync_onedrive_tokens";

/// 访问令牌距离过期不到这么多秒就先刷新（留出请求本身的耗时和时钟误差）。
pub const REFRESH_EARLY_SECS: i64 = 120;

/// 令牌放哪儿。生产是 [`DbTokenStore`]（DPAPI 加密进本机数据库），测试可以用 [`MemoryTokenStore`]。
pub trait TokenStore: Send + Sync {
    fn load(&self) -> Result<Option<TokenSet>, StorageError>;
    fn save(&self, tokens: &TokenSet) -> Result<(), StorageError>;
    fn clear(&self) -> Result<(), StorageError>;
}

/// 借用的存储也是存储（让调用方既能把存储交给 `Arc`，又能自己留着一份引用去查看）。
impl<T: TokenStore + ?Sized> TokenStore for &T {
    fn load(&self) -> Result<Option<TokenSet>, StorageError> {
        (**self).load()
    }

    fn save(&self, tokens: &TokenSet) -> Result<(), StorageError> {
        (**self).save(tokens)
    }

    fn clear(&self) -> Result<(), StorageError> {
        (**self).clear()
    }
}

fn store_error(err: impl std::fmt::Display) -> StorageError {
    // 错误文本来自数据库 / DPAPI，不含令牌。
    StorageError::Io(format!("读写本机保存的 OneDrive 登录信息失败：{err}"))
}

/// 本机数据库里的令牌存储（DPAPI 加密）。
pub struct DbTokenStore<'a> {
    db: &'a Database,
    sealer: &'a dyn SecretSealer,
}

impl<'a> DbTokenStore<'a> {
    pub fn new(db: &'a Database, sealer: &'a dyn SecretSealer) -> Self {
        Self { db, sealer }
    }
}

impl TokenStore for DbTokenStore<'_> {
    fn load(&self) -> Result<Option<TokenSet>, StorageError> {
        let plain = match open_from_setting(self.db, self.sealer, TOKENS_KEY) {
            Ok(Some(plain)) => plain,
            Ok(None) => return Ok(None),
            // 数据库本身出了问题：是暂时的故障，不能误判成“没登录”。
            Err(err @ (SyncError::Database(_) | SyncError::Io(_))) => return Err(store_error(err)),
            // 解不开（换了 Windows 账户 / 换了电脑拷来的数据库）或内容损坏：令牌用不了，
            // 按“没有登录”处理，界面会让用户重新登录。
            Err(_) => return Ok(None),
        };
        // 格式不对同样按“没有登录”处理（读不了 = 不确定，不崩）。
        Ok(serde_json::from_slice(&plain).ok())
    }

    fn save(&self, tokens: &TokenSet) -> Result<(), StorageError> {
        let plain = zeroize::Zeroizing::new(
            serde_json::to_vec(tokens).map_err(|err| store_error(err.to_string()))?,
        );
        seal_to_setting(self.db, self.sealer, TOKENS_KEY, &plain).map_err(store_error)
    }

    fn clear(&self) -> Result<(), StorageError> {
        clear_setting(self.db, TOKENS_KEY).map_err(store_error)
    }
}

/// 内存里的令牌存储（测试用，也给联网测试用环境变量里的刷新令牌起头）。
#[derive(Default)]
pub struct MemoryTokenStore {
    tokens: std::sync::Mutex<Option<TokenSet>>,
}

impl MemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(tokens: TokenSet) -> Self {
        Self {
            tokens: std::sync::Mutex::new(Some(tokens)),
        }
    }

    pub fn snapshot(&self) -> Option<TokenSet> {
        self.tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl TokenStore for MemoryTokenStore {
    fn load(&self) -> Result<Option<TokenSet>, StorageError> {
        Ok(self.snapshot())
    }

    fn save(&self, tokens: &TokenSet) -> Result<(), StorageError> {
        *self.tokens.lock().unwrap_or_else(|e| e.into_inner()) = Some(tokens.clone());
        Ok(())
    }

    fn clear(&self) -> Result<(), StorageError> {
        *self.tokens.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
}

/// 进程里同一时刻只让一个刷新在路上：微软换发新刷新令牌，两个请求拿着同一个旧令牌各刷一次，
/// 后存盘的会盖掉先存盘的，运气不好时存下的恰好是已经不用的那个。
static REFRESH_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

/// 给 Graph 请求提供有效的访问令牌。
#[derive(Clone)]
pub struct TokenManager<'a> {
    client: Client,
    endpoints: Endpoints,
    client_id: String,
    store: Arc<dyn TokenStore + 'a>,
}

impl<'a> TokenManager<'a> {
    pub fn new(
        client: Client,
        endpoints: Endpoints,
        client_id: String,
        store: Arc<dyn TokenStore + 'a>,
    ) -> Self {
        Self {
            client,
            endpoints,
            client_id,
            store,
        }
    }

    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    fn not_signed_in() -> StorageError {
        StorageError::AuthFailed("还没有登录 OneDrive，请先登录".to_owned())
    }

    /// 当前可用的访问令牌；快过期就先刷新。
    pub async fn access_token(&self) -> Result<String, StorageError> {
        let tokens = self.store.load()?.ok_or_else(Self::not_signed_in)?;
        if tokens.expires_at - now_secs() > REFRESH_EARLY_SECS {
            return Ok(tokens.access_token);
        }
        self.refresh(None).await
    }

    /// Graph 说这个访问令牌不行（401）：强制刷新一次。`rejected` 是被拒绝的那个令牌——
    /// 如果存储里已经是更新的令牌（别的请求刚刷新过），直接用它，不再重复刷新。
    pub async fn force_refresh(&self, rejected: &str) -> Result<String, StorageError> {
        self.refresh(Some(rejected)).await
    }

    async fn refresh(&self, rejected: Option<&str>) -> Result<String, StorageError> {
        let _guard = REFRESH_LOCK.lock().await;
        let current = self.store.load()?.ok_or_else(Self::not_signed_in)?;
        // 拿到锁之后重读：等锁的时候别人可能已经刷新好了。
        let still_good = match rejected {
            Some(rejected) => current.access_token != rejected,
            None => current.expires_at - now_secs() > REFRESH_EARLY_SECS,
        };
        if still_good {
            return Ok(current.access_token);
        }
        let fresh = oauth::refresh(
            &self.client,
            &self.endpoints,
            &self.client_id,
            &current.refresh_token,
        )
        .await
        .map_err(|err| match err {
            OAuthError::Reauth(_) => StorageError::AuthFailed(REAUTH_MESSAGE.to_owned()),
            other => StorageError::from(other),
        })?;
        // 先存盘再使用：新的刷新令牌丢了的话，下次只能拿旧的去刷（微软不会立刻作废旧的，
        // 但不能指望）。存不下就明确报错，不悄悄继续。
        self.store.save(&fresh)?;
        Ok(fresh.access_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::sealer::{DpapiSealer, TestSealer};
    use crate::sync::settings::{sync_mode, SyncMode};

    fn sample() -> TokenSet {
        TokenSet {
            access_token: "ACCESS-TOKEN-PLAINTEXT-123".to_owned(),
            refresh_token: "REFRESH-TOKEN-PLAINTEXT-456".to_owned(),
            expires_at: 1_900_000_000,
        }
    }

    fn open_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::Builder::new()
            .prefix("zssh-onedrive-tokens-")
            .tempdir()
            .expect("临时目录");
        let db = Database::open(&dir.path().join("zssh.db")).expect("打开库");
        (dir, db)
    }

    #[test]
    fn tokens_round_trip_and_are_not_stored_in_plaintext() {
        let (_dir, db) = open_db();
        let sealer = TestSealer(0x5a);
        let store = DbTokenStore::new(&db, &sealer);
        assert_eq!(store.load().unwrap(), None, "一开始没有登录");

        store.save(&sample()).unwrap();
        assert_eq!(store.load().unwrap(), Some(sample()));

        let raw = db
            .setting_get(TOKENS_KEY)
            .unwrap()
            .expect("设置表里有这一项");
        for secret in ["ACCESS-TOKEN-PLAINTEXT-123", "REFRESH-TOKEN-PLAINTEXT-456"] {
            assert!(!raw.contains(secret), "设置表里不能有明文令牌：{raw}");
        }

        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
        assert_eq!(
            db.setting_get(TOKENS_KEY).unwrap(),
            None,
            "断开连接要真的删掉"
        );
    }

    #[test]
    fn tokens_sealed_on_another_machine_read_as_signed_out() {
        // 换了台电脑（DPAPI 不同）或数据损坏：解不开就当没登录，不崩。
        let (_dir, db) = open_db();
        DbTokenStore::new(&db, &TestSealer(1))
            .save(&sample())
            .unwrap();
        let other = DbTokenStore::new(&db, &TestSealer(2));
        assert_eq!(other.load().unwrap(), None);
        db.setting_set(TOKENS_KEY, "\"not-base64!!\"").unwrap();
        assert_eq!(DbTokenStore::new(&db, &TestSealer(1)).load().unwrap(), None);
        db.setting_set(TOKENS_KEY, "123").unwrap();
        assert_eq!(DbTokenStore::new(&db, &TestSealer(1)).load().unwrap(), None);
    }

    #[test]
    fn the_token_key_never_goes_into_the_sync_package() {
        assert_eq!(sync_mode(TOKENS_KEY), Some(SyncMode::LocalOnly));
        assert!(!crate::sync::settings::is_synced(TOKENS_KEY));
    }

    #[test]
    fn real_dpapi_round_trip() {
        if !cfg!(windows) {
            return;
        }
        let (_dir, db) = open_db();
        let store = DbTokenStore::new(&db, &DpapiSealer);
        store.save(&sample()).unwrap();
        assert_eq!(store.load().unwrap(), Some(sample()));
        let raw = db.setting_get(TOKENS_KEY).unwrap().unwrap();
        assert!(!raw.contains("PLAINTEXT"), "{raw}");
    }

    #[test]
    fn memory_store_behaves_like_the_real_one() {
        let store = MemoryTokenStore::new();
        assert_eq!(store.load().unwrap(), None);
        store.save(&sample()).unwrap();
        assert_eq!(store.load().unwrap(), Some(sample()));
        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }
}
