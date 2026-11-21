//! oo7's encrypted file keyring, the backend Flatpak uses, as the store of
//! a device's sync key seeds (#37). The Secret Service backend needs a
//! session bus and keyring daemon, which CI does not run; it shares the
//! adapter code above the backend.

use matters_core::storage::membership::{DeviceSeeds, KeyStore, SyncStanding};
use matters_core::{Database, Task};
use matters_keyring::Oo7KeyStore;
use matters_sync::authority::Route;

fn seeds(byte: u8) -> DeviceSeeds {
    DeviceSeeds::from_bytes(&[byte; DeviceSeeds::BYTES]).unwrap()
}

fn id_of(seeds: &DeviceSeeds) -> [u8; 48] {
    seeds.keys().unwrap().id()
}

#[test]
fn seeds_round_trip_replace_and_persist_in_the_encrypted_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keyring.keyring");
    let secret = [0x5c; 64];
    {
        let store = Oo7KeyStore::file(&path, &secret).unwrap();
        assert!(store.load("absent").unwrap().is_none());
        store.store("device-a", &seeds(1)).unwrap();
        store.store("device-b", &seeds(2)).unwrap();
        store.store("device-a", &seeds(3)).unwrap(); // replaces
        assert_eq!(
            id_of(&store.load("device-a").unwrap().unwrap()),
            id_of(&seeds(3))
        );
        assert_eq!(
            id_of(&store.load("device-b").unwrap().unwrap()),
            id_of(&seeds(2))
        );
    }
    // Reopened from disk with the same secret.
    let store = Oo7KeyStore::file(&path, &secret).unwrap();
    assert_eq!(
        id_of(&store.load("device-a").unwrap().unwrap()),
        id_of(&seeds(3))
    );
    // The file never holds the seeds in clear.
    let raw = std::fs::read(&path).unwrap();
    assert!(
        !raw.windows(16).any(|w| w == [3; 16]),
        "seeds are encrypted"
    );
    // Another secret cannot read them.
    let wrong = Oo7KeyStore::file(&path, &[0x00; 64]);
    assert!(wrong.is_err() || wrong.unwrap().load("device-a").is_err());
}

#[test]
fn a_collection_resumes_with_keys_from_the_file_keyring() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("m.db");
    let keyring = dir.path().join("keyring.keyring");
    let secret = [0x7e; 64];
    let route = Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    };
    {
        let store = Oo7KeyStore::file(&keyring, &secret).unwrap();
        let db = Database::open(&db_path).unwrap();
        db.create_collection(&store, route).unwrap();
    }
    let store = Oo7KeyStore::file(&keyring, &secret).unwrap();
    let db = Database::open(&db_path).unwrap();
    assert_eq!(db.resume_sync(&store).unwrap(), SyncStanding::Active);
    db.add_task(&mut Task::new(uuid(), "sealed", 0.0)).unwrap();
    assert_eq!(db.outgoing_records().unwrap().len(), 1);
}

fn uuid() -> String {
    "00000000-0000-0000-0000-000000000001".into()
}
