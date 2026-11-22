//! The sync service contract's conformance scenarios
//! (docs/protocol/sync-relay-v0-conformance.json) against the real
//! `matters-relay` binary and a real PostgreSQL (#38). Also:
//! - an outage of the database, which must answer `unavailable` and
//!   acknowledge nothing;
//! - storage and log inspection: no plaintext reaches the database or the
//!   relay's logs, and the relay needs no key configuration.
//!
//! The scenarios are read from the fixture file, so it stays the single
//! source of truth.
//! - A "restart" kills the relay process.
//! - A "crash" starts it with a crash point (the `fault-injection`
//!   feature).
//! - A "backup" copies the database as a PostgreSQL template; a "restore"
//!   swaps that copy back and runs `matters-relay new-generation --all`.
//! - An expired token is a stored session whose expiry has passed.
#![cfg(feature = "fault-injection")]

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use futures_util::StreamExt;
use matters_sync::authority::{sign_session, DeviceKeys, Route};
use matters_sync::record::record_hash;
use serde_json::Value;
use sqlx::postgres::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const BIN: &str = env!("CARGO_BIN_EXE_matters-relay");
const MARKER: &[u8] = b"PLAINTEXT-MARKER";
const OPERATOR: &str = "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn route_hex(route: &Route) -> String {
    hex(&[route.application, route.tenant, route.collection].concat())
}

fn other_route() -> Route {
    Route {
        collection: [7; 16],
        ..route()
    }
}

/// A running relay process, and everything it wrote to stderr.
struct Process {
    child: Child,
    addr: String,
    log: Arc<Mutex<String>>,
}

fn start(url: &str, env: &[(&str, &str)]) -> Process {
    let mut command = Command::new(BIN);
    command
        .env("MATTERS_RELAY_DATABASE_URL", url)
        .env("MATTERS_RELAY_LISTEN", "127.0.0.1:0")
        .env("MATTERS_RELAY_OPERATOR_TOKEN", OPERATOR)
        .stderr(Stdio::piped());
    for (k, v) in env {
        command.env(k, v);
    }
    let mut child = command.spawn().expect("the relay binary");
    let stderr = child.stderr.take().unwrap();
    let log = Arc::new(Mutex::new(String::new()));
    let (tx, rx) = std::sync::mpsc::channel();
    let sink = log.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(addr) = line.strip_prefix("matters-relay: listening on ") {
                let _ = tx.send(addr.to_owned());
            }
            sink.lock().unwrap().push_str(&line);
            sink.lock().unwrap().push('\n');
        }
    });
    let addr = rx
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| panic!("the relay did not start: {}", log.lock().unwrap()));
    Process { child, addr, log }
}

impl Process {
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn cli(url: &str, args: &[&str]) {
    let status = Command::new(BIN)
        .env("MATTERS_RELAY_DATABASE_URL", url)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "matters-relay {args:?}");
}

/// A response, or the connection lost before one arrived.
struct Response {
    status: u16,
    body: Vec<u8>,
}

/// One HTTP/1.1 request on its own connection.
async fn http(
    addr: &str,
    method: &str,
    path: &str,
    token: Option<&[u8; 32]>,
    body: &[u8],
) -> Option<Response> {
    let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: relay\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(t) = token {
        head.push_str(&format!("Authorization: Bearer {}\r\n", hex(t)));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await.ok()?;
    stream.write_all(body).await.ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.ok()?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;
    let status = head.split(' ').nth(1)?.parse().ok()?;
    let mut body = raw[split + 4..].to_vec();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        body = dechunk(&body)?;
    }
    Some(Response { status, body })
}

fn dechunk(mut raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = raw.windows(2).position(|w| w == b"\r\n")?;
        let size = usize::from_str_radix(std::str::from_utf8(&raw[..end]).ok()?.trim(), 16).ok()?;
        raw = &raw[end + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(raw.get(..size)?);
        raw = raw.get(size + 2..)?;
    }
}

/// The body's map, as raw CBOR values by key.
fn fields(body: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut d = minicbor::Decoder::new(body);
    let mut out = BTreeMap::new();
    if let Ok(Some(n)) = d.map() {
        for _ in 0..n {
            let key = d.str().unwrap().to_owned();
            let start = d.position();
            d.skip().unwrap();
            out.insert(key, body[start..d.position()].to_vec());
        }
    }
    out
}

fn as_bytes(raw: &[u8]) -> Vec<u8> {
    minicbor::Decoder::new(raw).bytes().unwrap().to_vec()
}

fn as_uint(raw: &[u8]) -> u64 {
    minicbor::Decoder::new(raw).u64().unwrap()
}

fn as_text(raw: &[u8]) -> String {
    minicbor::Decoder::new(raw).str().unwrap().to_owned()
}

fn as_bool(raw: &[u8]) -> bool {
    minicbor::Decoder::new(raw).bool().unwrap()
}

fn page_records(raw: &[u8]) -> Vec<Vec<u8>> {
    let mut d = minicbor::Decoder::new(raw);
    let n = d.array().unwrap().unwrap();
    (0..n)
        .map(|_| {
            d.array().unwrap();
            d.u64().unwrap();
            d.bytes().unwrap().to_vec()
        })
        .collect()
}

fn session_request(route: &Route, device: &[u8; 48], extra: Option<(&[u8], &[u8])>) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    e.map(if extra.is_some() { 4 } else { 2 }).unwrap();
    e.str("route").unwrap();
    e.array(3)
        .unwrap()
        .bytes(&route.application)
        .unwrap()
        .bytes(&route.tenant)
        .unwrap()
        .bytes(&route.collection)
        .unwrap();
    e.str("device").unwrap().bytes(device).unwrap();
    if let Some((challenge, signature)) = extra {
        e.str("challenge").unwrap().bytes(challenge).unwrap();
        e.str("signature").unwrap().bytes(signature).unwrap();
    }
    e.into_writer()
}

/// A named record: its ID, bytes and the path ID it is uploaded under.
#[derive(Clone)]
struct Named {
    path_id: [u8; 32],
    bytes: Vec<u8>,
}

/// One scenario's world: the database, the relay, the devices and records.
struct World {
    db: TestDb,
    relay: Process,
    env: Vec<(String, String)>,
    b: DeviceKeys,
    c: DeviceKeys,
    collection: Collection,
    records: BTreeMap<String, Named>,
    controls: BTreeMap<String, Vec<u8>>,
    tokens: BTreeMap<String, [u8; 32]>,
    generations: BTreeMap<String, Vec<u8>>,
    cursors: BTreeMap<String, Vec<u8>>,
    backups: BTreeMap<String, String>,
    exports: BTreeMap<String, std::path::PathBuf>,
    second: Option<(TestDb, Process)>,
    hints: Option<Arc<Mutex<Vec<u64>>>>,
    scratch: tempfile::TempDir,
}

impl World {
    async fn new(config: Option<&Value>, wants_hints: bool) -> Option<Self> {
        let db = TestDb::start().await?;
        // A is the collection's admin, whose genesis the fixture signs.
        let mut collection = collection();
        let genesis = collection.genesis.clone();
        let b = device();
        let c = device();
        // Provision both routes; B joins the main one.
        db.relay
            .provision(&route(), collection.keys.root_key(), &genesis)
            .await
            .unwrap();
        let add = transition(
            &collection,
            vec![collection.admin.member(true), b.member(false)],
        );
        db.relay
            .put_authority(&route(), &record_hash(&add), &add)
            .await
            .unwrap();
        collection.authority.apply_membership(&add).unwrap();
        let other_keys = matters_sync::authority::RecoverySecret::new(fresh())
            .keys(&other_route())
            .unwrap();
        let other_genesis = matters_sync::authority::genesis(
            &other_route(),
            &other_keys,
            &collection.admin,
            &matters_sync::ContentKey::new([4; 32]),
            fresh(),
        )
        .unwrap();
        db.relay
            .provision(&other_route(), other_keys.root_key(), &other_genesis)
            .await
            .unwrap();
        let g0 = db
            .relay
            .records(&route(), None, 1)
            .await
            .unwrap()
            .generation;
        let mut env = Vec::new();
        if let Some(config) = config {
            if let Some(n) = config["max_records_per_route"].as_u64() {
                db.relay.set_record_quota(&route(), Some(n)).await.unwrap();
            }
            if config["rate_limit"] == "configured" {
                env.push(("MATTERS_RELAY_RATE".to_owned(), "1/4".to_owned()));
            }
        }
        let relay = start(&db.url(), &pairs(&env));
        let mut world = Self {
            db,
            relay,
            env,
            b,
            c,
            collection,
            records: BTreeMap::new(),
            controls: BTreeMap::new(),
            tokens: BTreeMap::new(),
            generations: BTreeMap::from([("g0".to_owned(), g0.to_vec())]),
            cursors: BTreeMap::new(),
            backups: BTreeMap::new(),
            exports: BTreeMap::new(),
            second: None,
            hints: None,
            scratch: tempfile::tempdir().unwrap(),
        };
        world.name_records();
        // The roster's devices hold sessions from the start, as a running
        // client would; steps about sessions make their own.
        for d in ["A", "B"] {
            world.token(d, &route(), false).await;
        }
        if wants_hints {
            world.subscribe_hints().await;
        }
        Some(world)
    }

    /// A named control message, made once so that every step sends the
    /// same bytes.
    fn control(&mut self, name: &str) -> Vec<u8> {
        if let Some(bytes) = self.controls.get(name) {
            return bytes.clone();
        }
        let c = &self.collection;
        let bytes = match name {
            "membership-removing-B" => transition(c, vec![c.admin.member(true)]),
            "membership-adding-C" => transition(
                c,
                vec![
                    c.admin.member(true),
                    self.b.member(false),
                    self.c.member(false),
                ],
            ),
            "writer-start-of-C" => matters_sync::authority::sign(
                &matters_sync::authority::Message {
                    route: route(),
                    signer: self.c.id(),
                    body: matters_sync::authority::Body::WriterStart(
                        matters_sync::authority::WriterStart {
                            manifest: c.authority.manifest(),
                            document: [4; 16],
                            device: self.c.id(),
                            incarnation: fresh(),
                            actor: 3,
                            previous: None,
                        },
                    ),
                },
                self.c.signing_key(),
                fresh(),
            )
            .unwrap(),
            other => panic!("control {other}"),
        };
        self.controls.insert(name.to_owned(), bytes.clone());
        bytes
    }

    fn device(&self, name: &str) -> &DeviceKeys {
        match name {
            "A" => &self.collection.admin,
            "B" => &self.b,
            "C" => &self.c,
            _ => panic!("device {name}"),
        }
    }

    /// The fixture's named records.
    fn name_records(&mut self) {
        let mut add =
            |name: &str, author: &DeviceKeys, route: &Route, id: [u8; 32], path: [u8; 32]| {
                let plaintext = [MARKER, b"-", name.as_bytes()].concat();
                self.records.insert(
                    name.to_owned(),
                    Named {
                        path_id: path,
                        bytes: record(author, id, &plaintext, route),
                    },
                );
            };
        let a = &self.collection.admin;
        let r1 = fresh();
        add("r1", a, &route(), r1, r1);
        add("r1'", a, &route(), r1, r1);
        for name in ["r2", "r3", "r4"] {
            let id = fresh();
            add(name, a, &route(), id, id);
        }
        let id = fresh();
        add("rB", &self.b, &route(), id, id);
        let id = fresh();
        let elsewhere = Route {
            collection: [9; 16],
            ..route()
        };
        add("r-other-collection", a, &elsewhere, id, id);
        let id = fresh();
        add("r-path-id-differs", a, &route(), id, fresh());
        let id = fresh();
        add("r-signed-by-B", &self.b, &route(), id, id);
        let id = fresh();
        add("r-unknown-suite", a, &route(), id, id);
        let named = self.records.get_mut("r-unknown-suite").unwrap();
        let at = named
            .bytes
            .windows(18)
            .position(|w| w == b"matters-sync-pq-v0")
            .unwrap();
        named.bytes[at + 17] = b'9';
    }

    async fn restart(&mut self, crash: Option<&str>) {
        self.relay.kill();
        let mut env = pairs(&self.env);
        if let Some(point) = crash {
            env.push(("MATTERS_RELAY_CRASH_AT", point));
        }
        self.relay = start(&self.db.url(), &env);
    }

    /// A session for `device` on `route` through the API, cached.
    async fn token(&mut self, device: &str, on: &Route, second: bool) -> [u8; 32] {
        let key = format!("{device}{}{}", route_hex(on), second);
        if let Some(t) = self.tokens.get(&key) {
            return *t;
        }
        let addr = self.addr(second);
        let keys = self.device(device);
        let c = http(
            &addr,
            "POST",
            "/v0/session/challenge",
            None,
            &session_request(on, &keys.id(), None),
        )
        .await
        .unwrap();
        assert_eq!(c.status, 200, "challenge for {device}");
        let challenge: [u8; 32] = as_bytes(&fields(&c.body)["challenge"]).try_into().unwrap();
        let signature = sign_session(keys, on, &challenge, fresh()).unwrap();
        let s = http(
            &addr,
            "POST",
            "/v0/session",
            None,
            &session_request(on, &keys.id(), Some((&challenge, &signature))),
        )
        .await
        .unwrap();
        assert_eq!(s.status, 200, "session for {device}");
        let token: [u8; 32] = as_bytes(&fields(&s.body)["token"]).try_into().unwrap();
        self.tokens.insert(key, token);
        token
    }

    fn addr(&self, second: bool) -> String {
        if second {
            self.second.as_ref().unwrap().1.addr.clone()
        } else {
            self.relay.addr.clone()
        }
    }

    async fn subscribe_hints(&mut self) {
        let token = self.token("B", &route(), false).await;
        let url = format!(
            "ws://{}/v0/routes/{}/hints",
            self.relay.addr,
            route_hex(&route())
        );
        let mut request = url.as_str().into_client_request().unwrap();
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", hex(&token)).parse().unwrap(),
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        tokio::spawn(async move {
            while let Some(Ok(message)) = socket.next().await {
                let body = message.into_data();
                if let Some(raw) = fields(&body).get("sequence") {
                    sink.lock().unwrap().push(as_uint(raw));
                }
            }
        });
        self.hints = Some(seen);
    }

    /// Compare a generation with a name: g0, a new name (bound now), or
    /// "not g0".
    fn check_generation(&mut self, expected: &str, actual: &[u8], context: &str) {
        if let Some(rest) = expected.strip_prefix("not ") {
            assert_ne!(self.generations[rest], actual, "{context}: generation");
            return;
        }
        let bound = self
            .generations
            .entry(expected.to_owned())
            .or_insert_with(|| actual.to_vec());
        assert_eq!(bound.as_slice(), actual, "{context}: generation {expected}");
    }

    fn check(&mut self, response: Option<&Response>, expect: &Value, context: &str) {
        let Some(response) = response else {
            assert_eq!(
                expect["status"], "connection-lost",
                "{context}: no response"
            );
            return;
        };
        assert_ne!(
            expect["status"], "connection-lost",
            "{context}: a response arrived"
        );
        let f = fields(&response.body);
        for (key, value) in expect.as_object().unwrap() {
            match key.as_str() {
                "status" => assert_eq!(
                    u64::from(response.status),
                    value.as_u64().unwrap(),
                    "{context}: {:?}",
                    f.get("code").map(|c| as_text(c))
                ),
                "sequence" | "high-water" => {
                    assert_eq!(
                        as_uint(&f[key]),
                        value.as_u64().unwrap(),
                        "{context}: {key}"
                    )
                }
                "code" => assert_eq!(as_text(&f["code"]), value.as_str().unwrap(), "{context}"),
                "retry" | "complete" => {
                    assert_eq!(
                        as_bool(&f[key]),
                        value.as_bool().unwrap(),
                        "{context}: {key}"
                    )
                }
                "retry-after-ms" => {
                    assert!(as_uint(&f[key]) > 0, "{context}: retry-after-ms")
                }
                "generation" => {
                    let actual = as_bytes(&f["generation"]);
                    self.check_generation(value.as_str().unwrap(), &actual, context);
                }
                "digest" => {
                    let name = value
                        .as_str()
                        .unwrap()
                        .trim_start_matches("digest(")
                        .trim_end_matches(')');
                    assert_eq!(
                        as_bytes(&f["digest"]),
                        record_hash(&self.records[name].bytes),
                        "{context}: digest"
                    );
                }
                "records" => {
                    let expected: Vec<Vec<u8>> = value
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|n| self.records[n.as_str().unwrap()].bytes.clone())
                        .collect();
                    assert_eq!(page_records(&f["records"]), expected, "{context}: records");
                }
                "save_cursor" => {
                    self.cursors
                        .insert(value.as_str().unwrap().to_owned(), as_bytes(&f["next"]));
                }
                // Checked by "records": the comparison is byte for byte.
                "bytes_equal" => assert!(value.as_bool().unwrap()),
                other => panic!("{context}: unknown expectation {other}"),
            }
        }
    }

    async fn step(&mut self, step: &Value, context: &str) {
        let expect = &step["expect"];
        let device = step["as"].as_str().unwrap_or("A").to_owned();
        if step.get("restart").is_some() {
            self.restart(None).await;
            return;
        }
        if let Some(name) = step["backup"].as_str() {
            self.relay.kill();
            self.db.relay.pool().close().await;
            let copy = format!("{}_{name}", self.db.name);
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "CREATE DATABASE {copy} TEMPLATE {}",
                self.db.name
            )))
            .execute(&self.db.admin)
            .await
            .unwrap();
            self.backups.insert(name.to_owned(), copy);
            self.db.relay = matters_relay::Relay::connect(&self.db.url()).await.unwrap();
            self.relay = start(&self.db.url(), &pairs(&self.env));
            return;
        }
        if let Some(name) = step["restore"].as_str() {
            self.relay.kill();
            self.db.relay.pool().close().await;
            let name_now = self.db.name.clone();
            for sql in [
                format!("DROP DATABASE {name_now} WITH (FORCE)"),
                format!("CREATE DATABASE {name_now} TEMPLATE {}", self.backups[name]),
            ] {
                sqlx::query(sqlx::AssertSqlSafe(sql))
                    .execute(&self.db.admin)
                    .await
                    .unwrap();
            }
            // The restore procedure, unless the scenario skips its last step.
            if step["keep_generation"] != true {
                cli(&self.db.url(), &["new-generation", "--all"]);
            }
            self.db.relay = matters_relay::Relay::connect(&self.db.url()).await.unwrap();
            self.relay = start(&self.db.url(), &pairs(&self.env));
            // Sessions opened after the backup are gone with it.
            self.tokens.clear();
            return;
        }
        if step["operator"] == "export" {
            let file = self.scratch.path().join(step["save"].as_str().unwrap());
            cli(
                &self.db.url(),
                &["export", &route_hex(&route()), file.to_str().unwrap()],
            );
            self.exports
                .insert(step["save"].as_str().unwrap().to_owned(), file);
            return;
        }
        if step["operator"] == "import" {
            let target = TestDb::start().await.unwrap();
            let file = &self.exports[step["from"].as_str().unwrap()];
            cli(&target.url(), &["import", file.to_str().unwrap()]);
            let process = start(&target.url(), &[]);
            self.second = Some((target, process));
            return;
        }
        if step.get("reconnect").is_some() {
            self.tokens.retain(|k, _| !k.starts_with(&device));
            return;
        }
        if let Some(n) = step["hints_received_for"].as_u64() {
            tokio::time::sleep(Duration::from_millis(700)).await;
            let seen = self.hints.as_ref().unwrap().lock().unwrap().clone();
            let count = seen.iter().filter(|s| **s == n).count() as u64;
            assert!(
                count <= expect["count_at_most"].as_u64().unwrap(),
                "{context}: {seen:?}"
            );
            return;
        }
        if !step["session"].is_null() {
            let keys = self.device(&device);
            let id = keys.id();
            let addr = self.relay.addr.clone();
            let c = http(
                &addr,
                "POST",
                "/v0/session/challenge",
                None,
                &session_request(&route(), &id, None),
            )
            .await
            .unwrap();
            if c.status != 200 {
                self.check(Some(&c), expect, context);
                return;
            }
            let challenge: [u8; 32] = as_bytes(&fields(&c.body)["challenge"]).try_into().unwrap();
            let signer = if step["session"]["signature"] == "by-B" {
                &self.b
            } else {
                self.device(&device)
            };
            let signature = sign_session(signer, &route(), &challenge, fresh()).unwrap();
            let body = session_request(&route(), &id, Some((&challenge, &signature)));
            let mut response = http(&addr, "POST", "/v0/session", None, &body).await;
            if step["session"]["challenge"] == "reused" {
                assert_eq!(response.unwrap().status, 200, "{context}: the first use");
                response = http(&addr, "POST", "/v0/session", None, &body).await;
            }
            self.check(response.as_ref(), expect, context);
            return;
        }
        if let Some(kind) = step["put_authority"].as_str() {
            let bytes = self.control(kind);
            let token = if step["anonymous"] == true {
                None
            } else {
                Some(self.token(&device, &route(), false).await)
            };
            let path = format!(
                "/v0/routes/{}/authority/{}",
                route_hex(&route()),
                hex(&record_hash(&bytes))
            );
            let response = http(
                &self.relay.addr.clone(),
                "PUT",
                &path,
                token.as_ref(),
                &bytes,
            )
            .await;
            self.check(response.as_ref(), expect, context);
            return;
        }
        if let Some(path) = step["raw_path"].as_str() {
            let path = path.replace("{route}", &route_hex(&route()));
            let token = self.token(&device, &route(), false).await;
            let response = http(&self.relay.addr.clone(), "GET", &path, Some(&token), b"").await;
            self.check(response.as_ref(), expect, context);
            return;
        }
        if step["burst"] == "over-rate-limit" {
            let token = self.token(&device, &route(), false).await;
            let path = format!("/v0/routes/{}/records", route_hex(&route()));
            for _ in 0..16 {
                let response = http(&self.relay.addr.clone(), "GET", &path, Some(&token), b"")
                    .await
                    .unwrap();
                if response.status == 429 {
                    self.check(Some(&response), expect, context);
                    return;
                }
            }
            panic!("{context}: no 429 within 16 requests");
        }
        if !step["append"].is_null() {
            let names: Vec<String> = match &step["append"] {
                Value::Array(list) => list
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned())
                    .collect(),
                v => vec![v.as_str().unwrap().to_owned()],
            };
            if let Some(point) = step["crash"].as_str() {
                self.restart(Some(point)).await;
            }
            let mut token = match step["token"].as_str() {
                Some("for-other-route") => self.token(&device, &other_route(), false).await,
                _ => self.token(&device, &route(), false).await,
            };
            if step["token"] == "expired" {
                sqlx::query("UPDATE sessions SET expires_at = 0 WHERE token_hash = $1")
                    .bind(record_hash(&token).as_slice())
                    .execute(self.db.relay.pool())
                    .await
                    .unwrap();
                self.tokens.clear();
            }
            for name in names {
                let named = self.records[&name].clone();
                let path = format!(
                    "/v0/routes/{}/records/{}",
                    route_hex(&route()),
                    hex(&named.path_id)
                );
                let mut response = http(
                    &self.relay.addr.clone(),
                    "PUT",
                    &path,
                    Some(&token),
                    &named.bytes,
                )
                .await;
                // A client signs in again when its session is gone (a restore).
                if step["token"].is_null() && response.as_ref().is_some_and(|r| r.status == 401) {
                    self.tokens.clear();
                    token = self.token(&device, &route(), false).await;
                    response = http(
                        &self.relay.addr.clone(),
                        "PUT",
                        &path,
                        Some(&token),
                        &named.bytes,
                    )
                    .await;
                }
                self.check(response.as_ref(), expect, &format!("{context} {name}"));
            }
            if step["crash"].is_string() {
                let status = self.relay.child.wait().unwrap();
                assert_eq!(status.code(), Some(86), "{context}: the crash point");
            }
            return;
        }
        if !step["fetch"].is_null() {
            let second = step["relay"] == "second-relay";
            let mut query = Vec::new();
            if let Some(name) = step["fetch"]["after"].as_str() {
                use base64::Engine;
                query.push(format!(
                    "after={}",
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&self.cursors[name])
                ));
            }
            if let Some(limit) = step["fetch"]["limit"].as_u64() {
                query.push(format!("limit={limit}"));
            }
            let path = format!(
                "/v0/routes/{}/records{}{}",
                route_hex(&route()),
                if query.is_empty() { "" } else { "?" },
                query.join("&")
            );
            let mut token = self.token(&device, &route(), second).await;
            let addr = self.addr(second);
            let mut response = http(&addr, "GET", &path, Some(&token), b"").await;
            if response.as_ref().is_some_and(|r| r.status == 401) {
                self.tokens.clear();
                token = self.token(&device, &route(), second).await;
                response = http(&addr, "GET", &path, Some(&token), b"").await;
            }
            self.check(response.as_ref(), expect, context);
            return;
        }
        panic!("{context}: unknown step {step}");
    }

    /// Every stored byte and every log line, for inspection.
    async fn inspect(&self) {
        let pool = PgPool::connect(&self.db.url()).await.unwrap();
        for table in ["records", "authority", "routes", "sessions", "challenges"] {
            let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT row_to_json({table})::text FROM {table}"
            )))
            .fetch_all(&pool)
            .await
            .unwrap();
            let dump = rows.join("\n");
            // bytea columns print as \x hex in JSON.
            assert!(!dump.contains(&hex(MARKER)), "{table} holds plaintext");
        }
        pool.close().await;
        let log = self.relay.log.lock().unwrap().clone();
        assert!(!log.contains("PLAINTEXT"), "the log holds plaintext");
        for token in self.tokens.values() {
            assert!(!log.contains(&hex(token)), "the log holds a session token");
        }
    }

    async fn finish(mut self) {
        self.relay.kill();
        if let Some((db, mut process)) = self.second.take() {
            process.kill();
            db.finish().await;
        }
        for copy in self.backups.values() {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP DATABASE {copy} WITH (FORCE)"
            )))
            .execute(&self.db.admin)
            .await
            .unwrap();
        }
        self.db.finish().await;
    }
}

fn pairs(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fn scenarios() -> Vec<Value> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../docs/protocol/sync-relay-v0-conformance.json"
    ))
    .unwrap();
    fixture["scenarios"].as_array().unwrap().clone()
}

/// The fixture's resource limits are the relay's.
#[test]
fn the_fixtures_limits_are_the_relays() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../docs/protocol/sync-relay-v0-conformance.json"
    ))
    .unwrap();
    let limits = &fixture["limits"];
    assert_eq!(
        limits["page_entries"].as_u64(),
        Some(u64::from(matters_relay::MAX_PAGE))
    );
    assert_eq!(
        limits["page_entry_bytes"].as_i64(),
        Some(matters_relay::MAX_PAGE_BYTES)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_conformance_scenario_passes_against_the_real_binary() {
    if std::env::var_os("MATTERS_RELAY_TEST_DATABASE_URL").is_none() {
        assert!(std::env::var_os("MATTERS_RELAY_REQUIRE_DB").is_none());
        eprintln!("skipped: no MATTERS_RELAY_TEST_DATABASE_URL");
        return;
    }
    let all = scenarios();
    assert_eq!(all.len(), 14, "every scenario of the fixture");
    for scenario in all {
        let name = scenario["name"].as_str().unwrap().to_owned();
        let wants_hints = scenario["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.get("hints_received_for").is_some());
        let mut world = World::new(scenario.get("config"), wants_hints)
            .await
            .unwrap();
        for (i, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
            world.step(step, &format!("{name} step {}", i + 1)).await;
        }
        world.inspect().await;
        world.finish().await;
        eprintln!("conformance: {name} passed");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_database_outage_answers_unavailable_and_acknowledges_nothing() {
    let Some(mut world) = World::new(None, false).await else {
        return;
    };
    let token = world.token("A", &route(), false).await;
    let r1 = world.records["r1"].clone();
    let path = format!(
        "/v0/routes/{}/records/{}",
        route_hex(&route()),
        hex(&r1.path_id)
    );
    let name = world.db.name.clone();
    for sql in [
        // Unlike a connection limit, this binds superusers too.
        format!("ALTER DATABASE {name} ALLOW_CONNECTIONS false"),
        format!(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
             WHERE datname = '{name}' AND pid <> pg_backend_pid()"
        ),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&world.db.admin)
            .await
            .unwrap();
    }
    let down = http(&world.relay.addr, "PUT", &path, Some(&token), &r1.bytes)
        .await
        .expect("an answer, not a dropped connection");
    assert_eq!(down.status, 503);
    let f = fields(&down.body);
    assert_eq!(as_text(&f["code"]), "unavailable");
    assert!(as_bool(&f["retry"]));
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER DATABASE {name} ALLOW_CONNECTIONS true"
    )))
    .execute(&world.db.admin)
    .await
    .unwrap();
    // The retry stores it now: nothing was acknowledged or kept before.
    let mut stored = None;
    for _ in 0..20 {
        let r = http(&world.relay.addr, "PUT", &path, Some(&token), &r1.bytes)
            .await
            .unwrap();
        if r.status != 503 {
            stored = Some(r);
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let stored = stored.expect("the relay recovers");
    assert_eq!(stored.status, 201);
    assert_eq!(as_uint(&fields(&stored.body)["sequence"]), 1);
    world.db.relay = matters_relay::Relay::connect(&world.db.url())
        .await
        .unwrap();
    world.inspect().await;
    world.finish().await;
}
