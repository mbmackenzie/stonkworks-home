fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("espidf") {
        embuild::espidf::sysenv::output();
        if std::env::var_os("CARGO_FEATURE_NETWORK_CHECK").is_some()
            || std::env::var_os("CARGO_FEATURE_CONTROLLER").is_some()
        {
            println!("cargo:rerun-if-env-changed=SONOS_DESK_CONFIG_DIR");
            let config_dir = std::env::var_os("SONOS_DESK_CONFIG_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
            for filename in ["config.local.toml", "broker-ca.local.pem"] {
                let source = config_dir.join(filename);
                println!("cargo:rerun-if-changed={}", source.display());
                std::fs::copy(source, out.join(filename))
                    .unwrap_or_else(|_| panic!("Provide {filename}; see config.example.toml"));
            }
        }
    }
}
