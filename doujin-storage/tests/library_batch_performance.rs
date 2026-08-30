use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use doujin_parser::PARSER_VERSION;
use doujin_parser::domain::ParseInput;
use doujin_parser::parser::parse_filename;
use doujin_scanner::{FilenameNormalization, MediaKind, PendingCollection, SourceKind};
use doujin_storage::CatalogRepository;
use doujin_storage::collections::CollectionQuery;

struct LargeFixture {
    root: PathBuf,
}

impl LargeFixture {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "doujin-storage-library-batch-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create fixture root");
        Self { root }
    }
}

impl Drop for LargeFixture {
    fn drop(&mut self) {
        if self
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("doujin-storage-library-batch-"))
        {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

#[test]
#[ignore = "manual 10,000+ collection acceptance benchmark"]
fn allowlisted_library_batches_are_bounded_and_complete_with_10_001_collections() {
    let fixture = LargeFixture::new();
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    let parsed = parse_filename(&ParseInput {
        filename: "[Circle] Large Fixture.zip".to_owned(),
        parody_evidence: Vec::new(),
    });
    let seed_started = Instant::now();
    for index in 0..10_001 {
        let path = fixture.root.join(format!("large-fixture-{index:05}.zip"));
        repository
            .ingest_collection(&PendingCollection {
                folder: fixture.root.clone(),
                path,
                root_path: fixture.root.clone(),
                root_label: "Large fixture".to_owned(),
                source: SourceKind::Archive,
                parser_version: PARSER_VERSION.to_owned(),
                parsed: parsed.clone(),
                filename_normalization: FilenameNormalization::Unchanged,
                media_kind: MediaKind::Zip,
            })
            .expect("ingest fixture collection");
    }
    assert_eq!(
        10_001,
        repository.collection_count().expect("collection count")
    );

    for per_page in [24, 48, 96, 192] {
        let started = Instant::now();
        let first = repository
            .collections(&CollectionQuery {
                per_page,
                ..CollectionQuery::default()
            })
            .expect("first page");
        let second = repository
            .collections(&CollectionQuery {
                page: 2,
                per_page,
                ..CollectionQuery::default()
            })
            .expect("second page");
        let elapsed = started.elapsed();
        assert_eq!(10_001, first.total);
        assert_eq!(
            usize::try_from(per_page).expect("page size"),
            first.items.len()
        );
        assert_eq!(
            usize::try_from(per_page).expect("page size"),
            second.items.len()
        );
        let first_ids = first
            .items
            .iter()
            .map(|item| item.id)
            .collect::<HashSet<_>>();
        assert!(
            second
                .items
                .iter()
                .all(|item| !first_ids.contains(&item.id))
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "two {per_page}-item queries took {elapsed:?}"
        );
        eprintln!("per_page={per_page}: first two queries {elapsed:?}");
    }

    let mut ids = HashSet::new();
    let mut page = 1;
    loop {
        let result = repository
            .collections(&CollectionQuery {
                page,
                per_page: 192,
                ..CollectionQuery::default()
            })
            .expect("complete pagination");
        if result.items.is_empty() {
            break;
        }
        for item in result.items {
            assert!(ids.insert(item.id), "duplicate collection {}", item.id);
        }
        page += 1;
    }
    assert_eq!(10_001, ids.len());
    eprintln!("seeded 10,001 collections in {:?}", seed_started.elapsed());
}
