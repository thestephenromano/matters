//! Shared test-only record-key derivation. Callers supply the validated/sender
//! transcript; independent Python vectors remain the oracle for these bytes.
use hpke_rs_crypto::{types::TwoStageKdfAlgorithm, HpkeCrypto};
type Provider = hpke_rs_libcrux::HpkeLibcrux<hpke_rs_libcrux::HpkeLibcruxPrng>;

pub fn content_key(
    id: &[u8; 32],
    cek: &[u8],
    info: &[u8],
) -> Result<Vec<u8>, hpke_rs_crypto::error::Error> {
    let prk = Provider::kdf_extract(TwoStageKdfAlgorithm::HkdfSha256, id, cek)?;
    Provider::kdf_expand(TwoStageKdfAlgorithm::HkdfSha256, &prk, info, 32)
}
