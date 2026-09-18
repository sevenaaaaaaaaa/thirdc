//! SigV4 黄金向量比对。
//!
//! 向量由本机 botocore（AWS 官方 Python SDK）在固定时间与 AWS 测试凭据下生成，
//! 见 tests/vectors/sigv4.json 的 _generator / _botocore_version。
use kernel_deploy::s3::{uri_encode_path, SignInput, SigV4};
use serde_json::Value;
use std::collections::BTreeMap;

fn vectors() -> Value {
    serde_json::from_str(include_str!("vectors/sigv4.json")).expect("vectors json")
}

#[test]
fn matches_botocore_golden_vectors() {
    let v = vectors();
    let creds = &v["_credentials"];
    let access = creds["access_key"].as_str().unwrap();
    let secret = creds["secret_key"].as_str().unwrap();
    let date = v["_timestamp"].as_str().unwrap();

    let mut checked = 0;
    for case in v["vectors"].as_array().expect("vectors") {
        let method = case["method"].as_str().unwrap();
        let region = case["region"].as_str().unwrap();
        let service = case["service"].as_str().unwrap();
        let payload_hash = case["payload_sha256"].as_str().unwrap();
        let expected_creq = case["canonical_request"].as_str().unwrap();
        let expected_sts = case["string_to_sign"].as_str().unwrap();
        let expected_sig = case["signature"].as_str().unwrap();
        let expected_auth = case["authorization"].as_str().unwrap();
        let canonical_uri = case["canonical_uri"].as_str().unwrap();
        let canonical_query = case["canonical_query"].as_str().unwrap();
        let url = case["url"].as_str().unwrap();

        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), {
            let (_, rest) = url.split_once("://").unwrap();
            rest.split(['/', '?']).next().unwrap().to_string()
        });
        headers.insert("x-amz-date".to_string(), date.to_string());
        if let Some(obj) = case["signed_headers"].as_object() {
            for (k, val) in obj {
                if k.eq_ignore_ascii_case("content-type") {
                    headers.insert("content-type".to_string(), val.as_str().unwrap().to_string());
                }
            }
        }

        let input = SignInput {
            method,
            canonical_uri,
            canonical_query,
            headers,
            payload_hash,
            amz_date: date,
        };
        let sig = SigV4::new(access, secret, region, service);

        assert_eq!(sig.canonical_request(&input), expected_creq, "canonical request：{url}");
        assert_eq!(sig.string_to_sign(&input, expected_creq), expected_sts, "string to sign：{url}");
        assert_eq!(sig.signature(&input, expected_creq), expected_sig, "signature：{url}");
        assert_eq!(sig.authorization(&input), expected_auth, "Authorization：{url}");
        checked += 1;
    }
    assert!(checked >= 6, "向量数量异常：{checked}");
}

#[test]
fn uri_encoder_matches_botocore_on_every_vector() {
    let v = vectors();
    for case in v["vectors"].as_array().unwrap() {
        let raw = case["raw_path"].as_str().unwrap();
        let expected = case["canonical_uri"].as_str().unwrap();
        assert_eq!(
            uri_encode_path(raw),
            expected,
            "路径编码与 botocore 不一致：{raw}"
        );
    }
}
