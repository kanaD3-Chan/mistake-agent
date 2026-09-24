    use super::*;
    use crate::kernel::plugin::services::{Domain, DomainIo, RelPath, StorageHandle, TmpIo};
    use std::sync::Arc;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mistake-agent-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::create_dir_all(dir.join("memory")).unwrap();
        dir
    }

    fn real_store(dir: &std::path::Path) -> Arc<FileStorage> {
        Arc::new(FileStorage::open(dir).unwrap())
    }

    // ---------- RelPath：白名单构造即校验（ADR-0042） ----------

    #[test]
    fn relpath_accepts_whitelist_segments() {
        for ok in ["gaokao_pool.json", "a..md", "a/b/c.md", "dir_1-2/file.txt", "pool.json"] {
            assert!(RelPath::parse(ok).is_ok(), "应接受：{ok}");
        }
    }

    #[test]
    fn relpath_rejects_traversal_vectors() {
        for bad in [
            "..", "../", "../x", "a/../b", "a/..", "....//", ".", "./a", "a/.", "a..", "..a",
            "a//b", "/abs", "a\\b", "a:b", "a b", "a\u{0000}b", "a\tb", "。", "a。b",
        ] {
            assert!(RelPath::parse(bad).is_err(), "应拒绝：{bad:?}");
        }
    }

    #[test]
    fn relpath_rejects_leading_trailing_dots() {
        // 尾点（Windows 别名陷阱）与首点（隐藏文件/…）都拒绝。
        for bad in ["a.", ".a", "...", "....", ".hidden"] {
            assert!(RelPath::parse(bad).is_err(), "应拒绝：{bad:?}");
        }
    }

    #[test]
    fn relpath_rejects_empty_and_unicode() {
        assert!(RelPath::parse("").is_err());
        assert!(RelPath::parse("数据/文件").is_err(), "非 ASCII 应拒绝（同形字符攻击面）");
    }

    // ---------- DomainIo：域逃逸（ADR-0042） ----------

    #[tokio::test]
    async fn domain_write_read_roundtrip_and_list() {
        let dir = temp_root("domain-roundtrip");
        let store = real_store(&dir);
        let rel = RelPath::parse("gaokao_pool.json").unwrap();
        DomainIo::write(store.as_ref(), Domain::Data, &rel, b"[]").await.unwrap();
        assert_eq!(DomainIo::read(store.as_ref(), Domain::Data, &rel).await.unwrap(), b"[]");
        let listed = DomainIo::list(store.as_ref(), Domain::Data).await.unwrap();
        assert_eq!(listed, vec!["gaokao_pool.json".to_string()]);
        DomainIo::remove(store.as_ref(), Domain::Data, &rel).await.unwrap();
        assert!(DomainIo::list(store.as_ref(), Domain::Data).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn domain_rejects_removal_of_domain_root() {
        let dir = temp_root("domain-root");
        let store = real_store(&dir);
        // RelPath 至少一段，无法表达"域根"；验证子树删除只删子树、不越出域根。
        let nested = RelPath::parse("sub/nested/file.md").unwrap();
        DomainIo::write(store.as_ref(), Domain::Data, &nested, b"x").await.unwrap();
        let sub = RelPath::parse("sub").unwrap();
        DomainIo::remove_tree(store.as_ref(), Domain::Data, &sub).await.unwrap();
        assert!(DomainIo::list(store.as_ref(), Domain::Data).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn domain_rejects_escape_outside_root() {
        let dir = temp_root("domain-escape");
        let store = real_store(&dir);
        // 域根外真实存在一个文件：直接经 trait 逃逸路径不可达（RelPath 无 `..`）。
        let outside = dir.join("outside.txt");
        std::fs::write(&outside, b"secret").unwrap();
        let rel = RelPath::parse("x.txt").unwrap();
        // write 会写到域内而非外面；断言外部文件未被触碰。
        DomainIo::write(store.as_ref(), Domain::Data, &rel, b"y").await.unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- TmpIo：系统 temp 白名单（ADR-0042） ----------

    #[tokio::test]
    async fn tmp_reads_only_staged_prefix_in_temp() {
        let dir = temp_root("tmp-whitelist");
        let store = real_store(&dir);
        let temp = std::env::temp_dir();
        let ok = temp.join(format!("mistake-agent-{}.png", uuid::Uuid::new_v4()));
        let no_prefix = temp.join(format!("other-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&ok, b"img").unwrap();
        std::fs::write(&no_prefix, b"img").unwrap();
        assert_eq!(TmpIo::read_staged(store.as_ref(), &ok.to_string_lossy()).await.unwrap(), b"img");
        assert!(TmpIo::read_staged(store.as_ref(), &no_prefix.to_string_lossy()).await.is_err(), "非前缀应拒绝");
        assert!(TmpIo::read_staged(store.as_ref(), "/etc/passwd").await.is_err(), "系统路径应拒绝");
        // 删除：只删白名单内的。
        TmpIo::remove_staged(store.as_ref(), &ok.to_string_lossy()).await.unwrap();
        assert!(!ok.exists());
        assert!(no_prefix.exists(), "非前缀文件不受影响");
        let _ = std::fs::remove_file(&no_prefix);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn storage_handle_semantic_methods_route_to_io() {
        let dir = temp_root("handle-semantics");
        let store = real_store(&dir);
        let handle = StorageHandle::new(store.clone()).with_io(store.clone(), store.clone());
        handle.write_data_file("pool.json", r#"[]"#).await.unwrap();
        assert_eq!(handle.read_data_file("pool.json").await.unwrap(), "[]");
        assert!(handle.read_data_file("a/../b").await.is_err(), "句柄层同样拒绝遍历");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn memory_domain_isolation_from_data() {
        let dir = temp_root("domain-isolation");
        let store = real_store(&dir);
        let mem_rel = RelPath::parse("math.md").unwrap();
        let data_rel = RelPath::parse("math.md").unwrap();
        DomainIo::write(store.as_ref(), Domain::Memory, &mem_rel, b"m").await.unwrap();
        DomainIo::write(store.as_ref(), Domain::Data, &data_rel, b"d").await.unwrap();
        assert_eq!(
            DomainIo::read(store.as_ref(), Domain::Memory, &mem_rel)
                .await
                .unwrap(),
            b"m"
        );
        assert_eq!(
            DomainIo::read(store.as_ref(), Domain::Data, &data_rel)
                .await
                .unwrap(),
            b"d"
        );
        // 同名不同域互不干扰。
        assert_eq!(
            DomainIo::list(store.as_ref(), Domain::Memory).await.unwrap(),
            vec!["math.md".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- 会话元数据：标题 / 激活 / 删除（ADR-0044 收尾） ----------

    #[tokio::test]
    async fn session_title_activate_and_remove_roundtrip() {
        use crate::kernel::agent::session::{SessionKey, SessionMeta, SessionStatus};
        use crate::kernel::plugin::services::SessionStore;

        let dir = temp_root("session-ops");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        let store = real_store(&dir);
        let key = SessionKey::new();
        let mut meta = SessionMeta::new(key);
        meta.status = SessionStatus::Archived;
        meta.archived_at = Some(chrono::Utc::now());
        store.create_session(&key, &meta).await.unwrap();

        // set_title：写盘可回读；空白串按清空处理。
        store.set_title(&key, Some("  线性代数复习  ")).await.unwrap();
        assert_eq!(
            store.get_session(&key).await.unwrap().unwrap().title.as_deref(),
            Some("线性代数复习")
        );
        store.set_title(&key, Some("   ")).await.unwrap();
        assert!(store.get_session(&key).await.unwrap().unwrap().title.is_none());

        // activate：状态复位为 Active 且清空归档时间。
        store.activate(&key).await.unwrap();
        let meta = store.get_session(&key).await.unwrap().unwrap();
        assert_eq!(meta.status, SessionStatus::Active);
        assert!(meta.archived_at.is_none());

        // remove_session：连 jsonl 文件一起删；重复删除报错。
        let path = dir.join("sessions").join(format!("{key}.jsonl"));
        assert!(path.exists());
        store.remove_session(&key).await.unwrap();
        assert!(store.get_session(&key).await.unwrap().is_none());
        assert!(store.read_all(&key).await.unwrap().is_empty());
        assert!(!path.exists(), "会话文件应一并删除");
        assert!(store.remove_session(&key).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
