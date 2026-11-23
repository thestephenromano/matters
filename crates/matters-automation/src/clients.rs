//! Automation clients, their tokens and permissions (#203).
//!
//! Each client (the command line, links, an MCP server, an agent) has its own
//! token. The app keeps only the token's [`Verifier`], its SHA-256, so a copy
//! of the app's data never holds a working token. A token is 32 bytes from
//! the operating system's generator, so a fast hash is enough: there is
//! nothing to guess. Clients and their permissions belong to this device and
//! are not synced; where they are stored is #200's to decide, behind
//! [`Clients`].

use std::fmt;

/// What a client may do. Every call except opening a view needs a token,
/// and each method needs one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Read views, find and get items.
    Read,
    /// Add tasks, projects, headings and checklist items.
    Create,
    /// Change, schedule, complete, move and order items.
    Organise,
    /// Create, rename and order areas.
    Areas,
    /// Move items to the Trash and put them back. Nothing is ever deleted
    /// permanently, and the Trash is never emptied.
    Trash,
}

impl Permission {
    const ALL: [Permission; 5] = [
        Permission::Read,
        Permission::Create,
        Permission::Organise,
        Permission::Areas,
        Permission::Trash,
    ];

    fn bit(self) -> u8 {
        1 << Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    pub fn name(self) -> &'static str {
        match self {
            Permission::Read => "Read",
            Permission::Create => "Create",
            Permission::Organise => "Organise",
            Permission::Areas => "Areas",
            Permission::Trash => "Trash",
        }
    }
}

/// A set of [`Permission`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Permissions(u8);

impl Permissions {
    pub fn of(permissions: &[Permission]) -> Self {
        Permissions(permissions.iter().fold(0, |bits, p| bits | p.bit()))
    }

    pub fn allows(self, permission: Permission) -> bool {
        self.0 & permission.bit() != 0
    }
}

/// The ID of the client `matters://` links use (#69). Whatever it was
/// allowed, it never trashes anything or changes areas (#203's fixed
/// limits): a link anyone can send must not be able to.
///
/// The ID is reserved: a client store gives it only to the Links client
/// it makes itself ([`MemoryClients::add_links`]), never to a client
/// added, imported or renamed, or that client would silently lose Trash
/// and Areas. #200's store must keep this.
pub const LINKS_CLIENT: &str = "links";

/// A client as the app knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub permissions: Permissions,
    /// A revoked client's token is refused with its own error, so the
    /// caller can tell the user to set it up again.
    pub revoked: bool,
}

/// The SHA-256 of a token: what the app stores.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Verifier(pub [u8; 32]);

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Verifier(..)")
    }
}

/// The text form's prefix, so a token is recognisable in a config file.
const PREFIX: &str = "matters1_";

/// A client's token, given to the client once and never stored by the app.
pub struct Token(String);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(..)")
    }
}

impl Token {
    /// A new token and its verifier.
    pub fn generate() -> std::io::Result<(Token, Verifier)> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
        let text = format!("{PREFIX}{}", hex(&bytes));
        let verifier = verifier_of(&text);
        Ok((Token(text), verifier))
    }

    /// The text the client presents.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The verifier of a presented token, or `None` if it is not a token's text.
pub fn verifier_of_presented(presented: &str) -> Option<Verifier> {
    let digits = presented.strip_prefix(PREFIX)?;
    (digits.len() == 64
        && digits
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    .then(|| verifier_of(presented))
}

fn verifier_of(text: &str) -> Verifier {
    Verifier(libcrux_sha2::sha256(text.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where the app's clients are kept (#200 decides the real store).
pub trait Clients: Send + Sync {
    /// The client whose token has this verifier, revoked or not.
    fn by_verifier(&self, verifier: &Verifier) -> Option<Client>;
}

/// Clients held in memory: for tests, and until #200's store exists.
#[derive(Default)]
pub struct MemoryClients(std::sync::Mutex<Vec<(Verifier, Client)>>);

impl MemoryClients {
    /// Add a client and return its token.
    pub fn add(&self, name: &str, permissions: Permissions) -> std::io::Result<Token> {
        let id = format!(
            "client-{}",
            self.0.lock().unwrap_or_else(|e| e.into_inner()).len() + 1
        );
        self.add_as(&id, name, permissions)
    }

    /// Add the Links client, under its reserved ID ([`LINKS_CLIENT`]).
    pub fn add_links(&self, permissions: Permissions) -> std::io::Result<Token> {
        self.add_as(LINKS_CLIENT, "Links", permissions)
    }

    fn add_as(&self, id: &str, name: &str, permissions: Permissions) -> std::io::Result<Token> {
        let (token, verifier) = Token::generate()?;
        let mut clients = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let id = id.to_owned();
        clients.push((
            verifier,
            Client {
                id,
                name: name.into(),
                permissions,
                revoked: false,
            },
        ));
        Ok(token)
    }

    pub fn revoke(&self, name: &str) {
        let mut clients = self.0.lock().unwrap_or_else(|e| e.into_inner());
        for (_, client) in clients.iter_mut().filter(|(_, c)| c.name == name) {
            client.revoked = true;
        }
    }
}

impl Clients for MemoryClients {
    fn by_verifier(&self, verifier: &Verifier) -> Option<Client> {
        let clients = self.0.lock().unwrap_or_else(|e| e.into_inner());
        clients
            .iter()
            .find(|(v, _)| v == verifier)
            .map(|(_, c)| c.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_distinct_and_only_their_exact_text_verifies() {
        let (a, va) = Token::generate().unwrap();
        let (b, vb) = Token::generate().unwrap();
        assert_ne!(a.as_str(), b.as_str());
        assert_ne!(va, vb);
        assert_eq!(verifier_of_presented(a.as_str()), Some(va));
        let upper = a.as_str().to_uppercase();
        assert_eq!(verifier_of_presented(&upper), None);
        assert_eq!(verifier_of_presented(&a.as_str()[1..]), None);
        assert_eq!(verifier_of_presented(""), None);
        assert_eq!(verifier_of_presented("matters1_zz"), None);
        assert!(!format!("{a:?}{va:?}").contains(&a.as_str()[PREFIX.len()..]));
    }

    #[test]
    fn permissions_are_a_set() {
        let p = Permissions::of(&[Permission::Read, Permission::Trash]);
        assert!(p.allows(Permission::Read) && p.allows(Permission::Trash));
        for other in [Permission::Create, Permission::Organise, Permission::Areas] {
            assert!(!p.allows(other), "{other:?}");
        }
        assert!(!Permissions::default().allows(Permission::Read));
    }
}
