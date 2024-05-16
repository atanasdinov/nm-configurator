use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context};
use log::{info, warn};
use nmstate::{InterfaceType, NetworkState};

use crate::types::{Host, Interface};
use crate::{ALL_NODES_DIR, ALL_NODES_FILE, HOST_MAPPING_FILE};

/// `NetworkConfig` contains the generated configurations in the
/// following format: `Vec<(config_file_name, config_content>)`
type NetworkConfig = Vec<(String, String)>;

/// Generate network configurations from all YAML files in the `config_dir`
/// and store the result *.nmconnection files and host mapping (if applicable) under `output_dir`.
pub(crate) fn generate(config_dir: &str, output_dir: &str) -> Result<(), anyhow::Error> {
    let files_count = fs::read_dir(config_dir)?.count();

    if files_count == 0 {
        return Err(anyhow!("Empty config directory"));
    } else if files_count == 1 {
        let path = Path::new(config_dir).join(ALL_NODES_FILE);
        if let Ok(contents) = fs::read_to_string(&path) {
            info!("Generating config from {path:?}...");

            let (_, config) = generate_config(contents)?;
            return store_network_config(output_dir, ALL_NODES_DIR, config)
                .context("Storing network config");
        };
    };

    for entry in fs::read_dir(config_dir)? {
        let entry = entry?;
        let path = entry.path();

        if entry.metadata()?.is_dir() {
            warn!("Ignoring unexpected dir: {path:?}");
            continue;
        }

        info!("Generating config from {path:?}...");

        let hostname = extract_hostname(&path)
            .and_then(OsStr::to_str)
            .ok_or_else(|| anyhow!("Invalid file path"))?
            .to_owned();

        let data = fs::read_to_string(&path).context("Reading network config")?;

        let (interfaces, config) = generate_config(data)?;

        store_network_config(output_dir, &hostname, config).context("Storing network config")?;
        store_network_mapping(output_dir, hostname, interfaces)
            .context("Storing network mapping")?;
    }

    Ok(())
}

fn extract_hostname(path: &Path) -> Option<&OsStr> {
    if path
        .extension()
        .is_some_and(|ext| ext == "yml" || ext == "yaml")
    {
        path.file_stem()
    } else {
        path.file_name()
    }
}

fn generate_config(data: String) -> Result<(Vec<Interface>, NetworkConfig), anyhow::Error> {
    let network_state = NetworkState::new_from_yaml(&data)?;

    let interfaces = extract_interfaces(&network_state);
    validate_interfaces(&interfaces)?;

    let config = network_state
        .gen_conf()?
        .get("NetworkManager")
        .ok_or_else(|| anyhow!("Invalid NM configuration"))?
        .to_owned();

    Ok((interfaces, config))
}

fn extract_interfaces(network_state: &NetworkState) -> Vec<Interface> {
    network_state
        .interfaces
        .iter()
        .filter(|i| i.iface_type() != InterfaceType::Loopback)
        .map(|i| Interface {
            logical_name: i.name().to_owned(),
            mac_address: i.base_iface().mac_address.clone(),
            interface_type: i.iface_type().to_string(),
        })
        .collect()
}

fn validate_interfaces(interfaces: &[Interface]) -> anyhow::Result<()> {
    let ethernet_interfaces: Vec<&Interface> = interfaces
        .iter()
        .filter(|i| i.interface_type == InterfaceType::Ethernet.to_string())
        .collect();

    if ethernet_interfaces.is_empty() {
        return Err(anyhow!("No Ethernet interfaces were provided"));
    }

    let ethernet_interfaces: Vec<String> = ethernet_interfaces
        .iter()
        .filter(|i| i.mac_address.is_none())
        .map(|i| i.logical_name.to_owned())
        .collect();

    if !ethernet_interfaces.is_empty() {
        return Err(anyhow!(
            "Detected Ethernet interfaces without a MAC address: {}",
            ethernet_interfaces.join(", ")
        ));
    };

    Ok(())
}

fn store_network_config(
    output_dir: &str,
    hostname: &str,
    config: NetworkConfig,
) -> Result<(), anyhow::Error> {
    let path = Path::new(output_dir).join(hostname);

    fs::create_dir_all(&path).context("Creating output dir")?;

    config.iter().try_for_each(|(filename, content)| {
        let path = path.join(filename);

        fs::write(path, content).context("Writing config file")
    })
}

fn store_network_mapping(
    output_dir: &str,
    hostname: String,
    interfaces: Vec<Interface>,
) -> Result<(), anyhow::Error> {
    let path = Path::new(output_dir);

    let mapping_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.join(HOST_MAPPING_FILE))?;

    let hosts = [Host {
        hostname,
        interfaces,
    }];

    serde_yaml::to_writer(mapping_file, &hosts).context("Writing mapping file")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use crate::generate_conf::{
        extract_hostname, extract_interfaces, generate, generate_config, validate_interfaces,
    };
    use crate::types::{Host, Interface};
    use crate::HOST_MAPPING_FILE;

    #[test]
    fn generate_successfully() -> Result<(), anyhow::Error> {
        let config_dir = "testdata/generate";
        let exp_output_path = Path::new("testdata/generate/expected");
        let out_dir = "_out";
        let output_path = Path::new("_out").join("node1");

        assert!(generate(config_dir, out_dir).is_ok());

        // verify contents of *.nmconnection files
        let exp_eth0_conn = fs::read_to_string(exp_output_path.join("eth0.nmconnection"))?;
        let exp_bridge_conn = fs::read_to_string(exp_output_path.join("bridge0.nmconnection"))?;
        let exp_lo_conn = fs::read_to_string(exp_output_path.join("lo.nmconnection"))?;
        let eth0_conn = fs::read_to_string(output_path.join("eth0.nmconnection"))?;
        let bridge_conn = fs::read_to_string(output_path.join("bridge0.nmconnection"))?;
        let lo_conn = fs::read_to_string(output_path.join("lo.nmconnection"))?;

        assert_eq!(exp_eth0_conn, eth0_conn);
        assert_eq!(exp_bridge_conn, bridge_conn);
        assert_eq!(exp_lo_conn, lo_conn);

        // verify contents of the host mapping file
        let mut exp_hosts: Vec<Host> = serde_yaml::from_str(
            fs::read_to_string(exp_output_path.join(HOST_MAPPING_FILE))?.as_str(),
        )?;
        let mut hosts: Vec<Host> = serde_yaml::from_str(
            fs::read_to_string(Path::new(out_dir).join(HOST_MAPPING_FILE))?.as_str(),
        )?;

        assert_eq!(exp_hosts.len(), hosts.len());

        exp_hosts.sort_by(|a, b| a.hostname.cmp(&b.hostname));
        hosts.sort_by(|a, b| a.hostname.cmp(&b.hostname));

        for (h1, h2) in exp_hosts.iter_mut().zip(hosts.iter_mut()) {
            h1.interfaces
                .sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
            h2.interfaces
                .sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
        }

        assert_eq!(exp_hosts, hosts);

        // cleanup
        fs::remove_dir_all(out_dir)?;

        Ok(())
    }

    #[test]
    fn generate_fails_due_to_empty_dir() {
        fs::create_dir_all("empty").unwrap();

        let error = generate("empty", "_out").unwrap_err();
        assert_eq!(error.to_string(), "Empty config directory");

        fs::remove_dir_all("empty").unwrap();
    }

    #[test]
    fn generate_fails_due_to_missing_path() {
        let error = generate("<missing>", "_out").unwrap_err();
        assert!(error.to_string().contains("No such file or directory"))
    }

    #[test]
    fn generate_config_fails_due_to_invalid_data() {
        let err = generate_config("<invalid>".to_string()).unwrap_err();
        assert!(err.to_string().contains("Invalid YAML string"))
    }

    #[test]
    fn extract_interfaces_skips_loopback() -> Result<(), serde_yaml::Error> {
        let net_state: nmstate::NetworkState = serde_yaml::from_str(
            r#"---
        interfaces:
          - name: eth1
            type: ethernet
            mac-address: FE:C4:05:42:8B:AA
          - name: bridge0
            type: linux-bridge
            mac-address: FE:C4:05:42:8B:AB
          - name: lo
            type: loopback
            mac-address: 00:00:00:00:00:00
        "#,
        )?;

        let mut interfaces = extract_interfaces(&net_state);
        interfaces.sort_by(|a, b| a.logical_name.cmp(&b.logical_name));

        assert_eq!(
            interfaces,
            vec![
                Interface {
                    logical_name: "bridge0".to_string(),
                    mac_address: Option::from("FE:C4:05:42:8B:AB".to_string()),
                    interface_type: "linux-bridge".to_string(),
                },
                Interface {
                    logical_name: "eth1".to_string(),
                    mac_address: Option::from("FE:C4:05:42:8B:AA".to_string()),
                    interface_type: "ethernet".to_string(),
                },
            ]
        );

        Ok(())
    }

    #[test]
    fn validate_interfaces_missing_ethernet_interfaces() {
        let interfaces = vec![
            Interface {
                logical_name: "eth3.1365".to_string(),
                mac_address: None,
                interface_type: "vlan".to_string(),
            },
            Interface {
                logical_name: "bond0".to_string(),
                mac_address: None,
                interface_type: "bond".to_string(),
            },
        ];

        let error = validate_interfaces(&interfaces).unwrap_err();
        assert_eq!(error.to_string(), "No Ethernet interfaces were provided")
    }

    #[test]
    fn validate_interfaces_missing_mac_addresses() {
        let interfaces = vec![
            Interface {
                logical_name: "eth0".to_string(),
                mac_address: Option::from("00:11:22:33:44:55".to_string()),
                interface_type: "ethernet".to_string(),
            },
            Interface {
                logical_name: "eth1".to_string(),
                mac_address: None,
                interface_type: "ethernet".to_string(),
            },
            Interface {
                logical_name: "eth2".to_string(),
                mac_address: Option::from("00:11:22:33:44:56".to_string()),
                interface_type: "ethernet".to_string(),
            },
            Interface {
                logical_name: "eth3".to_string(),
                mac_address: None,
                interface_type: "ethernet".to_string(),
            },
            Interface {
                logical_name: "eth3.1365".to_string(),
                mac_address: None,
                interface_type: "vlan".to_string(),
            },
            Interface {
                logical_name: "bond0".to_string(),
                mac_address: Option::from("00:11:22:33:44:58".to_string()),
                interface_type: "bond".to_string(),
            },
        ];

        let error = validate_interfaces(&interfaces).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Detected Ethernet interfaces without a MAC address: eth1, eth3"
        )
    }

    #[test]
    fn validate_interfaces_successfully() {
        let interfaces = vec![
            Interface {
                logical_name: "eth0".to_string(),
                mac_address: Option::from("00:11:22:33:44:55".to_string()),
                interface_type: "ethernet".to_string(),
            },
            Interface {
                logical_name: "eth0.1365".to_string(),
                mac_address: None,
                interface_type: "vlan".to_string(),
            },
            Interface {
                logical_name: "bond0".to_string(),
                mac_address: None,
                interface_type: "bond".to_string(),
            },
        ];

        assert!(validate_interfaces(&interfaces).is_ok())
    }

    #[test]
    fn extract_host_name() {
        assert_eq!(extract_hostname("".as_ref()), None);
        assert_eq!(extract_hostname("node1".as_ref()), Some("node1".as_ref()));
        assert_eq!(
            extract_hostname("node1.example".as_ref()),
            Some("node1.example".as_ref())
        );
        assert_eq!(
            extract_hostname("node1.example.com".as_ref()),
            Some("node1.example.com".as_ref())
        );
        assert_eq!(
            extract_hostname("node1.example.com.yml".as_ref()),
            Some("node1.example.com".as_ref())
        );
        assert_eq!(
            extract_hostname("node1.example.com.yaml".as_ref()),
            Some("node1.example.com".as_ref())
        );
    }
}
