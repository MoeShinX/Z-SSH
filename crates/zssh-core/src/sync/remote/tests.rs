use std::sync::{Arc, Mutex};

use super::*;

/// 一个可以调行为的内存“服务器”，不走网络。
#[derive(Clone)]
struct Fake {
    state: Arc<Mutex<FakeState>>,
    name: String,
}

struct FakeState {
    objects: std::collections::BTreeMap<String, Vec<u8>>,
    /// PUT 的响应里带不带 ETag。
    put_returns_etag: bool,
    /// 认不认条件头。
    honors_conditions: bool,
    /// 只认 If-Match，把 If-None-Match 当没看见。
    ignores_if_none_match: bool,
    gets: usize,
}

impl Fake {
    fn new(put_returns_etag: bool, honors_conditions: bool) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState {
                objects: Default::default(),
                put_returns_etag,
                honors_conditions,
                ignores_if_none_match: false,
                gets: 0,
            })),
            name: "main".to_owned(),
        }
    }

    fn etag(bytes: &[u8]) -> String {
        format!("\"{}\"", &content_hash(bytes)[..16])
    }

    fn external_write(&self, bytes: &[u8]) {
        self.state
            .lock()
            .unwrap()
            .objects
            .insert(self.name.clone(), bytes.to_vec());
    }
}

impl RawObjectStore for Fake {
    fn get<'a>(&'a self) -> BoxFuture<'a, Result<Option<RawObject>, StorageError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.gets += 1;
            Ok(state.objects.get(&self.name).map(|bytes| RawObject {
                bytes: bytes.clone(),
                etag: Some(Self::etag(bytes)),
            }))
        })
    }

    fn put<'a>(
        &'a self,
        bytes: &'a [u8],
        condition: &'a Condition,
    ) -> BoxFuture<'a, Result<RawPut, StorageError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            let current = state.objects.get(&self.name).map(|b| Self::etag(b));
            if state.honors_conditions {
                match condition {
                    Condition::IfNoneMatchAny
                        if current.is_some() && !state.ignores_if_none_match =>
                    {
                        return Ok(RawPut::PreconditionFailed);
                    }
                    Condition::IfMatch(expected) if current.as_ref() != Some(expected) => {
                        return Ok(RawPut::PreconditionFailed);
                    }
                    _ => {}
                }
            }
            state.objects.insert(self.name.clone(), bytes.to_vec());
            Ok(RawPut::Written {
                etag: state.put_returns_etag.then(|| Self::etag(bytes)),
            })
        })
    }

    fn delete<'a>(&'a self, condition: &'a Condition) -> BoxFuture<'a, Result<bool, StorageError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            let current = state.objects.get(&self.name).map(|b| Self::etag(b));
            if let Condition::IfMatch(expected) = condition {
                if state.honors_conditions && current.as_ref() != Some(expected) {
                    return Ok(false);
                }
            }
            state.objects.remove(&self.name);
            Ok(true)
        })
    }

    fn sibling(&self, object_name: &str) -> Box<dyn RawObjectStore + '_> {
        Box::new(Self {
            state: self.state.clone(),
            name: object_name.to_owned(),
        })
    }
}

#[tokio::test]
async fn put_without_an_etag_in_the_response_reads_it_back() {
    let fake = Fake::new(false, true);
    let store = ConditionalStore::new(fake.clone(), ConcurrencyMode::Conditional);
    let version = store.write(b"one", None).await.expect("首次写");
    assert_eq!(version, Fake::etag(b"one"));
    assert!(
        fake.state.lock().unwrap().gets >= 1,
        "要再读一次才拿得到 ETag"
    );
    let read = store.read().await.unwrap().unwrap();
    assert_eq!(read.version, version);
    let next = store.write(b"two", Some(&version)).await.expect("带对版本");
    assert_eq!(next, Fake::etag(b"two"));
}

#[tokio::test]
async fn conditional_mode_maps_precondition_failures_to_conflict() {
    let fake = Fake::new(true, true);
    let store = ConditionalStore::new(fake.clone(), ConcurrencyMode::Conditional);
    let version = store.write(b"one", None).await.unwrap();
    assert_eq!(
        store.write(b"again", None).await.unwrap_err(),
        StorageError::Conflict,
        "对象已存在时 If-None-Match 被拒绝"
    );
    fake.external_write(b"someone-else");
    assert_eq!(
        store.write(b"two", Some(&version)).await.unwrap_err(),
        StorageError::Conflict
    );
    assert_eq!(
        store.delete(Some(&version)).await.unwrap_err(),
        StorageError::Conflict
    );
}

#[tokio::test]
async fn read_before_write_compares_content_hashes() {
    // 不认条件头的服务器：版本号是内容哈希，写前再读一次比较。
    let fake = Fake::new(true, false);
    let store = ConditionalStore::new(fake.clone(), ConcurrencyMode::ReadBeforeWrite);
    let version = store.write(b"one", None).await.unwrap();
    assert_eq!(version, content_hash(b"one"));
    assert_eq!(store.read().await.unwrap().unwrap().version, version);

    fake.external_write(b"changed-by-someone");
    assert_eq!(
        store.write(b"two", Some(&version)).await.unwrap_err(),
        StorageError::Conflict,
        "读和写之间别人改了内容，哈希对不上"
    );
    assert_eq!(
        store.write(b"two", None).await.unwrap_err(),
        StorageError::Conflict,
        "云端已有对象但调用方以为没有"
    );
    let current = store.read().await.unwrap().unwrap().version;
    store
        .write(b"two", Some(&current))
        .await
        .expect("版本对上才写");
    store.delete(Some(&content_hash(b"two"))).await.unwrap();
    assert!(store.read().await.unwrap().is_none());
}

#[tokio::test]
async fn probe_distinguishes_all_server_behaviours() {
    let honest = Fake::new(true, true);
    let probe = probe_concurrency(&honest).await.unwrap();
    assert_eq!(probe.mode, ConcurrencyMode::Conditional, "{probe:?}");
    assert!(
        honest.state.lock().unwrap().objects.is_empty(),
        "探测对象要删掉"
    );

    let ignoring = Fake::new(true, false);
    let probe = probe_concurrency(&ignoring).await.unwrap();
    assert_eq!(probe.mode, ConcurrencyMode::ReadBeforeWrite);
    assert!(probe
        .notes
        .iter()
        .any(|n| n.contains("忽略了 If-None-Match")));

    let half = Fake::new(true, true);
    half.state.lock().unwrap().ignores_if_none_match = true;
    let probe = probe_concurrency(&half).await.unwrap();
    assert_eq!(
        probe.mode,
        ConcurrencyMode::ReadBeforeWrite,
        "只认一半也不算支持"
    );
}

#[tokio::test]
async fn connection_probe_reports_read_write_and_existing_data_separately() {
    let fake = Fake::new(true, true);
    let first = probe_connection(&fake).await;
    assert!(first.read.ok && first.write.ok && !first.has_existing_data);
    assert!(first.concurrency.unwrap().mode.is_protected());

    fake.external_write(b"already-there");
    let second = probe_connection(&fake).await;
    assert!(second.has_existing_data);
    assert_eq!(
        fake.state
            .lock()
            .unwrap()
            .objects
            .get("main")
            .map(Vec::as_slice),
        Some(b"already-there".as_slice()),
        "探测不能动真正的同步包"
    );
}
