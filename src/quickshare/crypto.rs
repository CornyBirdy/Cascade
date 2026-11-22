//! Quick Share's encryption, as Google's Nearby protocol has it: a UKEY2
//! handshake (P-256 Diffie-Hellman, committed to with SHA-512), keys derived
//! with HKDF-SHA256, the 4-digit PIN both screens show, and every message
//! after it sealed as a SecureMessage (AES-256-CBC, signed with HMAC-SHA256).

use aes::Aes256;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::{EncodedPoint, PublicKey, SecretKey};
use sha2::{Digest, Sha256, Sha512};

use super::proto::{self, Msg};

/// `n` random bytes.
pub fn random(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    let _ = getrandom::getrandom(&mut b);
    b
}

pub fn random_i64() -> i64 {
    let b = random(8);
    i64::from_le_bytes(b.try_into().unwrap_or([7; 8]))
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn sha512(data: &[u8]) -> Vec<u8> {
    Sha512::digest(data).to_vec()
}

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
    m.update(data);
    m.finalize().into_bytes().into()
}

/// HKDF-SHA256 (extract with `salt`, expand with `info`) to `len` bytes.
pub fn hkdf(ikm: &[u8], salt: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let h = hkdf::Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = vec![0u8; len];
    let _ = h.expand(info, &mut out);
    out
}

/// A P-256 key pair for one handshake.
pub struct KeyPair {
    secret: SecretKey,
}

/// A coordinate as Java's BigInteger writes it (what the other side
/// expects): big-endian, a zero byte in front if the top bit is set.
fn signed(b: &[u8]) -> Vec<u8> {
    let mut v = b.to_vec();
    if v.first().is_some_and(|&x| x & 0x80 != 0) {
        v.insert(0, 0);
    }
    v
}

/// A coordinate read back: any leading zero bytes dropped, then padded to 32.
fn unsigned32(b: &[u8]) -> Option<[u8; 32]> {
    let start = b.iter().position(|&x| x != 0).unwrap_or(b.len());
    let b = &b[start..];
    if b.len() > 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out[32 - b.len()..].copy_from_slice(b);
    Some(out)
}

impl KeyPair {
    pub fn new() -> KeyPair {
        loop {
            if let Ok(secret) = SecretKey::from_slice(&random(32)) {
                return KeyPair { secret };
            }
        }
    }

    /// The public key as a GenericPublicKey message (EC_P256: x, y).
    pub fn public_message(&self) -> Vec<u8> {
        let point = self.secret.public_key().to_encoded_point(false);
        let (x, y) =
            (point.x().map(|v| v.to_vec()).unwrap_or_default(), point.y().map(|v| v.to_vec()).unwrap_or_default());
        Msg::new().int(1, 1).msg(2, Msg::new().bytes(1, &signed(&x)).bytes(2, &signed(&y))).build()
    }

    /// The shared secret with the other side's GenericPublicKey: SHA-256 of
    /// the Diffie-Hellman point's x.
    pub fn shared(&self, peer: &[u8]) -> Option<[u8; 32]> {
        let ec = proto::bytes(peer, 2)?;
        let (x, y) = (unsigned32(proto::bytes(ec, 1)?)?, unsigned32(proto::bytes(ec, 2)?)?);
        let point = EncodedPoint::from_affine_coordinates(&x.into(), &y.into(), false);
        let public = Option::<PublicKey>::from(PublicKey::from_encoded_point(&point))?;
        let dh = p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), public.as_affine());
        Some(sha256(dh.raw_secret_bytes()))
    }
}

/// The keys for one connection, one way and the other, and the PIN.
pub struct Keys {
    enc: Vec<u8>,
    dec: Vec<u8>,
    send_mac: Vec<u8>,
    recv_mac: Vec<u8>,
    pub pin: String,
}

/// The PIN both sides show, from the handshake's auth string.
pub fn pin(auth: &[u8]) -> String {
    let (mut hash, mut mult) = (0i64, 1i64);
    for &b in auth {
        hash = (hash + (b as i8) as i64 * mult) % 9973;
        mult = (mult * 31) % 9973;
    }
    format!("{:04}", hash.abs())
}

/// UKEY2's keys from the shared secret and the two handshake messages (as
/// sent): `server` for the side that was connected to.
pub fn derive(secret: &[u8; 32], client_init: &[u8], server_init: &[u8], server: bool) -> Keys {
    let info = [client_init, server_init].concat();
    let auth = hkdf(secret, b"UKEY2 v1 auth", &info, 32);
    let next = hkdf(secret, b"UKEY2 v1 next", &info, 32);
    const D2D_SALT: [u8; 32] = [
        0x82, 0xAA, 0x55, 0xA0, 0xD3, 0x97, 0xF8, 0x83, 0x46, 0xCA, 0x1C, 0xEE, 0x8D, 0x39, 0x09, 0xB9, 0x5F, 0x13,
        0xFA, 0x7D, 0xEB, 0x1D, 0x4A, 0xB3, 0x83, 0x76, 0xB8, 0x25, 0x6D, 0xA8, 0x55, 0x10,
    ];
    let client = hkdf(&next, &D2D_SALT, b"client", 32);
    let server_key = hkdf(&next, &D2D_SALT, b"server", 32);
    let smsg_salt = sha256(b"SecureMessage");
    let enc_of = |k: &[u8]| hkdf(k, &smsg_salt, b"ENC:2", 32);
    let mac_of = |k: &[u8]| hkdf(k, &smsg_salt, b"SIG:1", 32);
    let (mine, theirs) = if server { (&server_key, &client) } else { (&client, &server_key) };
    Keys { enc: enc_of(mine), send_mac: mac_of(mine), dec: enc_of(theirs), recv_mac: mac_of(theirs), pin: pin(&auth) }
}

impl Keys {
    /// `body` (a DeviceToDeviceMessage) sealed as a SecureMessage.
    pub fn seal(&self, body: &[u8]) -> Vec<u8> {
        let iv = random(16);
        let enc = cbc::Encryptor::<Aes256>::new_from_slices(&self.enc, &iv)
            .expect("32-byte key, 16-byte iv")
            .encrypt_padded_vec_mut::<Pkcs7>(body);
        // GcmMetadata: DEVICE_TO_DEVICE_MESSAGE, version 1.
        let metadata = Msg::new().int(1, 13).int(2, 1);
        let header = Msg::new().int(1, 1).int(2, 2).bytes(5, &iv).msg(6, metadata);
        let header_and_body = Msg::new().msg(1, header).bytes(2, &enc).build();
        let sig = hmac(&self.send_mac, &header_and_body);
        Msg::new().bytes(1, &header_and_body).bytes(2, &sig).build()
    }

    /// A SecureMessage opened: its signature checked, its body decrypted.
    pub fn open(&self, smsg: &[u8]) -> Option<Vec<u8>> {
        let hb = proto::bytes(smsg, 1)?;
        let sig = proto::bytes(smsg, 2)?;
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&self.recv_mac).ok()?;
        m.update(hb);
        m.verify_slice(sig).ok()?;
        let iv = proto::bytes(proto::bytes(hb, 1)?, 5)?;
        let body = proto::bytes(hb, 2)?;
        cbc::Decryptor::<Aes256>::new_from_slices(&self.dec, iv).ok()?.decrypt_padded_vec_mut::<Pkcs7>(body).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_agree() {
        let (a, b) = (KeyPair::new(), KeyPair::new());
        let (sa, sb) = (a.shared(&b.public_message()).unwrap(), b.shared(&a.public_message()).unwrap());
        assert_eq!(sa, sb);
        let server = derive(&sa, b"ci", b"si", true);
        let client = derive(&sb, b"ci", b"si", false);
        assert_eq!(server.pin, client.pin);
        assert_eq!(server.pin.len(), 4);
        let sealed = client.seal(b"hello there");
        assert_eq!(server.open(&sealed).as_deref(), Some(&b"hello there"[..]));
        // Tampered: refused.
        let mut bad = sealed.clone();
        let n = bad.len();
        bad[n - 1] ^= 1;
        assert!(server.open(&bad).is_none());
    }

    #[test]
    fn known_hkdf() {
        // RFC 5869 test case 1.
        let ikm = [0x0b; 22];
        let salt: Vec<u8> = (0..=0x0c).collect();
        let info: Vec<u8> = (0xf0..=0xf9).collect();
        let okm = hkdf(&ikm, &salt, &info, 42);
        assert_eq!(&okm[..4], &[0x3c, 0xb2, 0x5f, 0x25]);
    }
}
