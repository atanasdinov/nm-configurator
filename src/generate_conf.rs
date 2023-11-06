use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context};
use log::{info, warn};
use nmstate::{InterfaceType, NetworkState};
use serde::{Deserialize, Serialize};

const HOST_MAPPING_FILE: &str = "host_config.yaml";

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct HostInterfaces {
    hostname: String,
    interfaces: Vec<Interface>,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct Interface {
    logical_name: String,
    mac_address: String,
}

/// NetworkConfig contains the generated configurations in the
/// following format: Vec<(config_file_name, config_content>)
type NetworkConfig = Vec<(String, String)>;

/// Generate network configurations from all YAML files in the `config_dir`
/// and store the result *.nmconnection files and host mapping under `output_dir`.
pub(crate) fn generate(config_dir: &str, output_dir: &str) -> Result<(), anyhow::Error> {
    for entry in fs::read_dir(config_dir)? {
        let entry = entry?;
        let path = entry.path();

        if entry.metadata()?.is_dir() {
            warn!("Ignoring unexpected dir: {path:?}");
            continue;
        }

        info!("Generating config from {path:?}...");

        let hostname = path
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or_else(|| anyhow!("Invalid file path"))?
            .to_string();

        let data = fs::read_to_string(&path).with_context(|| "Reading network config")?;

        let (interfaces, config) = generate_config(data)?;

        store_network_config(output_dir, hostname, interfaces, config)
            .with_context(|| "Storing config")?;
    }

    Ok(())
}

fn generate_config(data: String) -> Result<(Vec<Interface>, NetworkConfig), anyhow::Error> {
    let network_state = NetworkState::new_from_yaml(&data)?;

    let interfaces = extract_interfaces(&network_state);
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
        .filter(|i| i.base_iface().mac_address.is_some())
        .map(|i| Interface {
            logical_name: i.name().to_string(),
            mac_address: i.base_iface().mac_address.clone().unwrap(),
        })
        .collect()
}

fn store_network_config(
    output_dir: &str,
    hostname: String,
    interfaces: Vec<Interface>,
    config: NetworkConfig,
) -> Result<(), anyhow::Error> {
    let path = Path::new(output_dir);

    fs::create_dir_all(path.join(&hostname)).with_context(|| "Creating output dir")?;

    config.iter().try_for_each(|(filename, content)| {
        let path = path.join(&hostname).join(filename);

        fs::write(path, content).with_context(|| "Writing config file")
    })?;

    let mapping_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.join(HOST_MAPPING_FILE))?;

    let host_interfaces = [HostInterfaces {
        hostname,
        interfaces,
    }];

    serde_yaml::to_writer(mapping_file, &host_interfaces).with_context(|| "Writing mapping file")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use crate::generate_conf::{
        extract_interfaces, generate, generate_config, HostInterfaces, HOST_MAPPING_FILE,
    };

    #[test]
    fn generate_successfully() -> Result<(), anyhow::Error> {
        let config_dir = "testdata/generate";
        let exp_output_path = Path::new("testdata/generate/expected");
        let out_dir = "_out";
        let output_path = Path::new("_out").join("node1");

        assert_eq!(generate(config_dir, out_dir).is_ok(), true);

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
        let mut exp_host_interfaces: Vec<HostInterfaces> = serde_yaml::from_str(
            fs::read_to_string(exp_output_path.join(HOST_MAPPING_FILE))?.as_str(),
        )?;
        let mut host_interfaces: Vec<HostInterfaces> = serde_yaml::from_str(
            fs::read_to_string(Path::new(out_dir).join(HOST_MAPPING_FILE))?.as_str(),
        )?;

        assert_eq!(exp_host_interfaces.len(), host_interfaces.len());

        exp_host_interfaces.sort_by(|a, b| a.hostname.cmp(&b.hostname));
        host_interfaces.sort_by(|a, b| a.hostname.cmp(&b.hostname));

        for (x, y) in exp_host_interfaces
            .iter_mut()
            .zip(host_interfaces.iter_mut())
        {
            x.interfaces
                .sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
            y.interfaces
                .sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
        }

        assert_eq!(exp_host_interfaces, host_interfaces);

        // cleanup
        fs::remove_dir_all(out_dir)?;

        Ok(())
    }

    #[test]
    fn generate_fails_due_to_missing_path() {
        let error = generate("<missing>", "_out").unwrap_err();
        assert_eq!(
            error.to_string().contains("No such file or directory"),
            true
        )
    }

    #[test]
    fn generate_config_fails_due_to_invalid_data() {
        let err = generate_config("<invalid>".to_string()).unwrap_err();
        assert_eq!(
            err.to_string()
                .contains("InvalidArgument: Invalid YAML string"),
            true
        )
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
        assert_eq!(interfaces.len(), 2);

        interfaces.sort_by(|a, b| a.logical_name.cmp(&b.logical_name));

        let i1 = interfaces.get(0).unwrap();
        assert_eq!(i1.logical_name, "bridge0");
        assert_eq!(i1.mac_address, "FE:C4:05:42:8B:AB");

        let i2 = interfaces.get(1).unwrap();
        assert_eq!(i2.logical_name, "eth1");
        assert_eq!(i2.mac_address, "FE:C4:05:42:8B:AA");

        Ok(())
    }
}
