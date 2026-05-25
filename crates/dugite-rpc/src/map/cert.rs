//! `dugite_primitives::Certificate` → `utxorpc.v1beta.cardano.Certificate`.
//!
//! Covers every Cardano certificate variant from Shelley through Conway:
//! stake registration / deregistration (legacy + Conway with deposit),
//! delegation (stake / vote / combined), pool registration / retirement,
//! genesis key delegation (Byron-era), MIR (Shelley), DRep
//! register / unregister / update, committee hot-auth / cold-resign.

use crate::map::common::{coin_bigint, hash_bytes, signed_bigint};
use crate::proto::v1beta::cardano as pb;
use dugite_primitives::credentials::Credential;
use dugite_primitives::transaction::{
    Anchor, Certificate, DRep, MIRSource, MIRTarget, PoolMetadata, PoolParams,
    Rational as DRational, Relay,
};

/// Map a single `Certificate` variant to the protobuf oneof.
///
/// The proto's `Certificate.redeemer` field is left `None` here — the
/// Plutus redeemer is attached during tx mapping (it's a per-tx
/// concern, not per-cert), see `map/tx.rs`'s witness mapper.
pub fn certificate_to_proto(cert: &Certificate) -> pb::Certificate {
    use pb::certificate::Certificate as PbCertOneof;
    let cert_oneof: PbCertOneof = match cert {
        Certificate::StakeRegistration(cred) => {
            PbCertOneof::StakeRegistration(credential_to_proto(cred))
        }
        Certificate::StakeDeregistration(cred) => {
            PbCertOneof::StakeDeregistration(credential_to_proto(cred))
        }
        Certificate::ConwayStakeRegistration {
            credential,
            deposit,
        } => PbCertOneof::RegCert(pb::RegCert {
            stake_credential: Some(credential_to_proto(credential)),
            coin: Some(coin_bigint(deposit.0)),
        }),
        Certificate::ConwayStakeDeregistration { credential, refund } => {
            PbCertOneof::UnregCert(pb::UnRegCert {
                stake_credential: Some(credential_to_proto(credential)),
                coin: Some(coin_bigint(refund.0)),
            })
        }
        Certificate::StakeDelegation {
            credential,
            pool_hash,
        } => PbCertOneof::StakeDelegation(pb::StakeDelegationCert {
            stake_credential: Some(credential_to_proto(credential)),
            pool_keyhash: hash_bytes(pool_hash),
        }),
        Certificate::PoolRegistration(params) => {
            PbCertOneof::PoolRegistration(pool_params_to_proto(params))
        }
        Certificate::PoolRetirement { pool_hash, epoch } => {
            PbCertOneof::PoolRetirement(pb::PoolRetirementCert {
                pool_keyhash: hash_bytes(pool_hash),
                epoch: *epoch,
            })
        }
        Certificate::RegDRep {
            credential,
            deposit,
            anchor,
        } => PbCertOneof::RegDrepCert(pb::RegDRepCert {
            drep_credential: Some(credential_to_proto(credential)),
            coin: Some(coin_bigint(deposit.0)),
            anchor: anchor.as_ref().map(anchor_to_proto),
        }),
        Certificate::UnregDRep { credential, refund } => {
            PbCertOneof::UnregDrepCert(pb::UnRegDRepCert {
                drep_credential: Some(credential_to_proto(credential)),
                coin: Some(coin_bigint(refund.0)),
            })
        }
        Certificate::UpdateDRep { credential, anchor } => {
            PbCertOneof::UpdateDrepCert(pb::UpdateDRepCert {
                drep_credential: Some(credential_to_proto(credential)),
                anchor: anchor.as_ref().map(anchor_to_proto),
            })
        }
        Certificate::VoteDelegation { credential, drep } => {
            PbCertOneof::VoteDelegCert(pb::VoteDelegCert {
                stake_credential: Some(credential_to_proto(credential)),
                drep: Some(drep_to_proto(drep)),
            })
        }
        Certificate::StakeVoteDelegation {
            credential,
            pool_hash,
            drep,
        } => PbCertOneof::StakeVoteDelegCert(pb::StakeVoteDelegCert {
            stake_credential: Some(credential_to_proto(credential)),
            pool_keyhash: hash_bytes(pool_hash),
            drep: Some(drep_to_proto(drep)),
        }),
        Certificate::RegStakeDeleg {
            credential,
            pool_hash,
            deposit,
        } => PbCertOneof::StakeRegDelegCert(pb::StakeRegDelegCert {
            stake_credential: Some(credential_to_proto(credential)),
            pool_keyhash: hash_bytes(pool_hash),
            coin: Some(coin_bigint(deposit.0)),
        }),
        Certificate::VoteRegDeleg {
            credential,
            drep,
            deposit,
        } => PbCertOneof::VoteRegDelegCert(pb::VoteRegDelegCert {
            stake_credential: Some(credential_to_proto(credential)),
            drep: Some(drep_to_proto(drep)),
            coin: Some(coin_bigint(deposit.0)),
        }),
        Certificate::RegStakeVoteDeleg {
            credential,
            pool_hash,
            drep,
            deposit,
        } => PbCertOneof::StakeVoteRegDelegCert(pb::StakeVoteRegDelegCert {
            stake_credential: Some(credential_to_proto(credential)),
            pool_keyhash: hash_bytes(pool_hash),
            drep: Some(drep_to_proto(drep)),
            coin: Some(coin_bigint(deposit.0)),
        }),
        Certificate::CommitteeHotAuth {
            cold_credential,
            hot_credential,
        } => PbCertOneof::AuthCommitteeHotCert(pb::AuthCommitteeHotCert {
            committee_cold_credential: Some(credential_to_proto(cold_credential)),
            committee_hot_credential: Some(credential_to_proto(hot_credential)),
        }),
        Certificate::CommitteeColdResign {
            cold_credential,
            anchor,
        } => PbCertOneof::ResignCommitteeColdCert(pb::ResignCommitteeColdCert {
            committee_cold_credential: Some(credential_to_proto(cold_credential)),
            anchor: anchor.as_ref().map(anchor_to_proto),
        }),
        Certificate::GenesisKeyDelegation {
            genesis_hash,
            genesis_delegate_hash,
            vrf_keyhash,
        } => PbCertOneof::GenesisKeyDelegation(pb::GenesisKeyDelegationCert {
            genesis_hash: hash_bytes(genesis_hash),
            genesis_delegate_hash: hash_bytes(genesis_delegate_hash),
            vrf_keyhash: hash_bytes(vrf_keyhash),
        }),
        Certificate::MoveInstantaneousRewards { source, target } => {
            PbCertOneof::MirCert(mir_to_proto(source, target))
        }
    };

    pb::Certificate {
        certificate: Some(cert_oneof),
        redeemer: None,
    }
}

pub fn credential_to_proto(c: &Credential) -> pb::StakeCredential {
    use pb::stake_credential::StakeCredential as Inner;
    let inner = match c {
        Credential::VerificationKey(h) => Inner::AddrKeyHash(h.as_ref().to_vec()),
        Credential::Script(h) => Inner::ScriptHash(h.as_ref().to_vec()),
    };
    pb::StakeCredential {
        stake_credential: Some(inner),
    }
}

pub fn drep_to_proto(d: &DRep) -> pb::DRep {
    use pb::d_rep::Drep as Inner;
    let inner = match d {
        DRep::KeyHash(h) => Inner::AddrKeyHash(h.as_ref().to_vec()),
        DRep::ScriptHash(h) => Inner::ScriptHash(h.as_ref().to_vec()),
        DRep::Abstain => Inner::Abstain(true),
        DRep::NoConfidence => Inner::NoConfidence(true),
    };
    pb::DRep { drep: Some(inner) }
}

pub fn anchor_to_proto(a: &Anchor) -> pb::Anchor {
    pb::Anchor {
        url: a.url.clone(),
        content_hash: hash_bytes(&a.data_hash),
    }
}

fn pool_params_to_proto(p: &PoolParams) -> pb::PoolRegistrationCert {
    pb::PoolRegistrationCert {
        operator: p.operator.as_ref().to_vec(),
        vrf_keyhash: hash_bytes(&p.vrf_keyhash),
        pledge: Some(coin_bigint(p.pledge.0)),
        cost: Some(coin_bigint(p.cost.0)),
        margin: Some(rational_to_proto(&p.margin)),
        reward_account: p.reward_account.clone(),
        pool_owners: p.pool_owners.iter().map(|h| h.as_ref().to_vec()).collect(),
        relays: p.relays.iter().map(relay_to_proto).collect(),
        pool_metadata: p.pool_metadata.as_ref().map(pool_metadata_to_proto),
    }
}

fn relay_to_proto(r: &Relay) -> pb::Relay {
    match r {
        Relay::SingleHostAddr { port, ipv4, ipv6 } => pb::Relay {
            ip_v4: ipv4.map(|b| b.to_vec()).unwrap_or_default(),
            ip_v6: ipv6.map(|b| b.to_vec()).unwrap_or_default(),
            dns_name: String::new(),
            port: port.map(u32::from).unwrap_or_default(),
        },
        Relay::SingleHostName { port, dns_name } => pb::Relay {
            ip_v4: Vec::new(),
            ip_v6: Vec::new(),
            dns_name: dns_name.clone(),
            port: port.map(u32::from).unwrap_or_default(),
        },
        Relay::MultiHostName { dns_name } => pb::Relay {
            ip_v4: Vec::new(),
            ip_v6: Vec::new(),
            dns_name: dns_name.clone(),
            port: 0,
        },
    }
}

fn pool_metadata_to_proto(m: &PoolMetadata) -> pb::PoolMetadata {
    pb::PoolMetadata {
        url: m.url.clone(),
        hash: hash_bytes(&m.hash),
    }
}

fn rational_to_proto(r: &DRational) -> pb::RationalNumber {
    pb::RationalNumber {
        numerator: r.numerator as i32,
        denominator: r.denominator as u32,
    }
}

fn mir_to_proto(source: &MIRSource, target: &MIRTarget) -> pb::MirCert {
    let pb_source = match source {
        MIRSource::Reserves => pb::MirSource::Reserves as i32,
        MIRSource::Treasury => pb::MirSource::Treasury as i32,
    };
    let (to, other_pot): (Vec<pb::MirTarget>, u64) = match target {
        MIRTarget::StakeCredentials(items) => {
            let to = items
                .iter()
                .map(|(cred, delta)| pb::MirTarget {
                    stake_credential: Some(credential_to_proto(cred)),
                    delta_coin: Some(signed_bigint(*delta)),
                })
                .collect();
            (to, 0)
        }
        MIRTarget::OtherAccountingPot(c) => (Vec::new(), *c),
    };
    pb::MirCert {
        from: pb_source,
        to,
        other_pot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::hash::{Hash28, Hash32};
    use dugite_primitives::value::Lovelace;

    fn cred() -> Credential {
        Credential::VerificationKey(Hash28::from_bytes([7u8; 28]))
    }
    fn cred_script() -> Credential {
        Credential::Script(Hash28::from_bytes([9u8; 28]))
    }

    #[test]
    fn stake_registration_roundtrip() {
        let cert = Certificate::StakeRegistration(cred());
        let pb_cert = certificate_to_proto(&cert);
        match pb_cert.certificate.unwrap() {
            pb::certificate::Certificate::StakeRegistration(c) => {
                match c.stake_credential.unwrap() {
                    pb::stake_credential::StakeCredential::AddrKeyHash(h) => {
                        assert_eq!(h, vec![7u8; 28]);
                    }
                    other => panic!("expected addr_key_hash, got {other:?}"),
                }
            }
            other => panic!("expected stake_registration, got {other:?}"),
        }
    }

    #[test]
    fn conway_stake_registration_carries_deposit() {
        let cert = Certificate::ConwayStakeRegistration {
            credential: cred(),
            deposit: Lovelace(2_000_000),
        };
        let pb_cert = certificate_to_proto(&cert);
        match pb_cert.certificate.unwrap() {
            pb::certificate::Certificate::RegCert(c) => {
                let coin = c.coin.unwrap();
                match coin.big_int.unwrap() {
                    pb::big_int::BigInt::Int(v) => assert_eq!(v, 2_000_000),
                    o => panic!("{o:?}"),
                }
            }
            other => panic!("expected reg_cert, got {other:?}"),
        }
    }

    #[test]
    fn stake_delegation_roundtrip() {
        let cert = Certificate::StakeDelegation {
            credential: cred_script(),
            pool_hash: Hash28::from_bytes([0xAA; 28]),
        };
        let pb_cert = certificate_to_proto(&cert);
        match pb_cert.certificate.unwrap() {
            pb::certificate::Certificate::StakeDelegation(c) => {
                assert_eq!(c.pool_keyhash, vec![0xAA; 28]);
                match c.stake_credential.unwrap().stake_credential.unwrap() {
                    pb::stake_credential::StakeCredential::ScriptHash(h) => {
                        assert_eq!(h, vec![9u8; 28]);
                    }
                    other => panic!("expected script_hash, got {other:?}"),
                }
            }
            other => panic!("expected stake_delegation, got {other:?}"),
        }
    }

    #[test]
    fn drep_variants_round_trip() {
        let cases = vec![
            (DRep::KeyHash(Hash32::from_bytes([1u8; 32])), true, false),
            (
                DRep::ScriptHash(Hash28::from_bytes([2u8; 28])),
                false,
                false,
            ),
            (DRep::Abstain, false, true),
            (DRep::NoConfidence, false, true),
        ];
        for (d, _is_keyhash, _is_special) in cases {
            // Just verify it doesn't panic and emits a valid oneof.
            let pb_d = drep_to_proto(&d);
            assert!(pb_d.drep.is_some());
        }
    }

    #[test]
    fn pool_registration_round_trips_relays_and_metadata() {
        let params = PoolParams {
            operator: Hash28::from_bytes([0xAA; 28]),
            vrf_keyhash: Hash32::from_bytes([0xBB; 32]),
            pledge: Lovelace(500_000_000),
            cost: Lovelace(340_000_000),
            margin: DRational {
                numerator: 1,
                denominator: 20,
            },
            reward_account: vec![0xE0; 29],
            pool_owners: vec![Hash28::from_bytes([0xC0; 28])],
            relays: vec![Relay::SingleHostAddr {
                port: Some(3001),
                ipv4: Some([192, 168, 1, 1]),
                ipv6: None,
            }],
            pool_metadata: Some(PoolMetadata {
                url: "https://example.com/pool.json".into(),
                hash: Hash32::from_bytes([0xDD; 32]),
            }),
        };
        let cert = Certificate::PoolRegistration(params);
        let pb_cert = certificate_to_proto(&cert);
        match pb_cert.certificate.unwrap() {
            pb::certificate::Certificate::PoolRegistration(c) => {
                assert_eq!(c.operator, vec![0xAA; 28]);
                assert_eq!(c.pool_owners.len(), 1);
                assert_eq!(c.relays.len(), 1);
                assert_eq!(c.relays[0].port, 3001);
                assert_eq!(c.relays[0].ip_v4, vec![192, 168, 1, 1]);
                let meta = c.pool_metadata.unwrap();
                assert_eq!(meta.url, "https://example.com/pool.json");
            }
            other => panic!("expected pool_registration, got {other:?}"),
        }
    }
}
