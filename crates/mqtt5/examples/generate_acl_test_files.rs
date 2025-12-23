use std::fs::File;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use mqtt5::broker::auth::PasswordAuthProvider;

    let alice_hash = PasswordAuthProvider::hash_password("alice123")?;
    let bob_hash = PasswordAuthProvider::hash_password("bob123")?;
    let charlie_hash = PasswordAuthProvider::hash_password("charlie123")?;

    let passwd_content = format!("alice:{alice_hash}\nbob:{bob_hash}\ncharlie:{charlie_hash}\n");

    let mut passwd_file = File::create("/tmp/mqtt_test_passwd.txt")?;
    passwd_file.write_all(passwd_content.as_bytes())?;

    println!("Password file created: /tmp/mqtt_test_passwd.txt");
    println!("ACL file already exists: /tmp/mqtt_test_acl.txt");

    Ok(())
}
