// SHA-384 of the complete signed record is the protocol chain-head definition.
pub type Id = [u8; 32];
pub type Hash = [u8; 48];
pub fn hash(bytes: &[u8]) -> Hash {
    use sha2::{Digest, Sha384};
    Sha384::digest(bytes).into()
}
