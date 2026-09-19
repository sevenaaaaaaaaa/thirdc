use kernel_deploy::encrypted_backup;

#[test]
fn encrypt_decrypt_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("Notes/sub")).unwrap();
    std::fs::write(dir.path().join("Notes/a.md"), "# 测试\n\n内容").unwrap();
    std::fs::write(dir.path().join("Notes/sub/b.md"), "更多内容").unwrap();
    std::fs::write(dir.path().join("thirdc.toml"), "name = \"kb\"").unwrap();

    let data = encrypted_backup::encrypt_vault(dir.path(), "mypassword123").unwrap();
    assert!(data.len() > 100, "加密后应该有实际内容");
    // 密文不等于明文
    let text = String::from_utf8_lossy(&data);
    assert!(!text.contains("测试"), "密文不应包含明文");

    let decrypted = encrypted_backup::decrypt_vault(&data, "mypassword123").unwrap();
    let s = String::from_utf8_lossy(&decrypted);
    assert!(s.contains("测试"), "解密后应包含明文: {}", &s[..80.min(s.len())]);
    assert!(s.contains("更多内容"));

    // 错误密码 → HMAC 校验失败
    assert!(encrypted_backup::decrypt_vault(&data, "wrongpass").is_err());
}
