use anyhow::{anyhow, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use scarb_metadata::{
    CompilationUnitMetadata, Metadata, PackageId, PackageMetadata, TargetMetadata,
};
use semver::VersionReq;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::str::FromStr;
use universal_sierra_compiler_api::{compile_sierra_at_path, SierraType};

pub use command::*;

mod command;
pub mod metadata;
pub mod version;

#[derive(Deserialize, Debug, PartialEq, Clone)]
struct StarknetArtifacts {
    version: u32,
    contracts: Vec<StarknetContract>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, PartialEq, Clone)]
struct StarknetContract {
    id: String,
    package_name: String,
    contract_name: String,
    artifacts: StarknetContractArtifactPaths,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, PartialEq, Clone)]
struct StarknetContractArtifactPaths {
    sierra: Utf8PathBuf,
}

/// Contains compiled Starknet artifacts
#[derive(Debug, PartialEq, Clone)]
pub struct StarknetContractArtifacts {
    /// Compiled sierra code
    pub sierra: String,
    /// Compiled casm code
    pub casm: String,
}

impl StarknetContractArtifacts {
    fn from_scarb_contract_artifact(
        starknet_contract: &StarknetContract,
        base_path: &Utf8Path,
    ) -> Result<Self> {
        let sierra_path = base_path.join(starknet_contract.artifacts.sierra.clone());
        let sierra = fs::read_to_string(sierra_path)?;

        let casm = compile_sierra_at_path(
            starknet_contract.artifacts.sierra.as_str(),
            Some(base_path.as_std_path()),
            &SierraType::Contract,
        )?;

        Ok(Self { sierra, casm })
    }
}

/// Get deserialized contents of `starknet_artifacts.json` file generated by Scarb
///
/// # Arguments
///
/// * `path` - A path to `starknet_artifacts.json` file.
fn artifacts_for_package(path: &Utf8Path) -> Result<StarknetArtifacts> {
    let starknet_artifacts =
        fs::read_to_string(path).with_context(|| format!("Failed to read {path:?} contents"))?;
    let starknet_artifacts: StarknetArtifacts =
        serde_json::from_str(starknet_artifacts.as_str())
            .with_context(|| format!("Failed to parse {path:?} contents. Make sure you have enabled sierra code generation in Scarb.toml"))?;
    Ok(starknet_artifacts)
}

#[derive(PartialEq, Debug)]
struct ContractArtifactData {
    path: Utf8PathBuf,
    test_type: Option<String>,
}

fn get_starknet_artifacts_paths_from_test_targets(
    target_dir: &Utf8Path,
    test_targets: &HashMap<String, &TargetMetadata>,
) -> Vec<ContractArtifactData> {
    let starknet_artifacts_file_name =
        |name: &str, metadata: &TargetMetadata| -> Option<ContractArtifactData> {
            let path = format!("{name}.test.starknet_artifacts.json");
            let path = target_dir.join(&path);
            let path = if path.exists() { Some(path) } else { None };

            let test_type = metadata
                .params
                .get("test-type")
                .and_then(|value| value.as_str())
                .unwrap();

            path.map(|path| ContractArtifactData {
                path: Utf8PathBuf::from_str(path.as_str()).unwrap(),
                test_type: Some(test_type.to_string()),
            })
        };

    test_targets
        .iter()
        .filter_map(|(target_name, metadata)| starknet_artifacts_file_name(target_name, metadata))
        .collect()
}

/// Try getting the path to `starknet_artifacts.json` file that is generated by `scarb build` or `scarb build --test` commands.
/// If contract artifacts are produced as part of the test target and exist in both `unittest` and `integrationtest`, then the path to `integrationtest` will be returned.
/// If the file is not present, `None` is returned.
fn get_starknet_artifacts_path(
    target_dir: &Utf8Path,
    target_name: &str,
) -> Option<ContractArtifactData> {
    let path = format!("{target_name}.starknet_artifacts.json");
    let path = target_dir.join(&path);
    let path = if path.exists() { Some(path) } else { None };

    path.map(|path| ContractArtifactData {
        path,
        test_type: None,
    })
}

/// Get the map with `StarknetContractArtifacts` for the given package
pub fn get_contracts_artifacts_and_source_sierra_paths(
    metadata: &Metadata,
    target_dir: &Utf8Path,
    package: &PackageMetadata,
    use_test_target_contracts: bool,
) -> Result<HashMap<String, (StarknetContractArtifacts, Utf8PathBuf)>> {
    let contracts_paths = if use_test_target_contracts {
        let test_targets = test_targets_by_name(package);
        get_starknet_artifacts_paths_from_test_targets(target_dir, &test_targets)
    } else {
        let target_name = target_name_for_package(metadata, &package.id)?;
        get_starknet_artifacts_path(target_dir, &target_name)
            .into_iter()
            .collect()
    };

    if contracts_paths.is_empty() {
        Ok(HashMap::default())
    } else {
        load_contracts_artifacts(&contracts_paths)
    }
}

fn load_contracts_artifacts(
    contracts_paths: &[ContractArtifactData],
) -> Result<HashMap<String, (StarknetContractArtifacts, Utf8PathBuf)>> {
    if contracts_paths.is_empty() {
        return Ok(HashMap::new());
    }

    // TODO use const
    let base_artifacts = contracts_paths
        .iter()
        .find(|paths| paths.test_type == Some("integration".to_string()))
        .unwrap_or(
            contracts_paths
                .first()
                .expect("Must have at least one value because of the assert above"),
        );

    let other_artifacts: Vec<&ContractArtifactData> = contracts_paths
        .iter()
        .filter(|path| path != &base_artifacts)
        .collect();

    let mut base_artifacts =
        load_contracts_artifacts_and_source_sierra_paths(&base_artifacts.path)?;

    for artifact in other_artifacts {
        let artifact = load_contracts_artifacts_and_source_sierra_paths(&artifact.path)?;
        for (key, value) in artifact {
            base_artifacts.entry(key).or_insert(value);
        }
    }

    Ok(base_artifacts)
}

fn load_contracts_artifacts_and_source_sierra_paths(
    contracts_path: &Utf8PathBuf,
) -> Result<HashMap<String, (StarknetContractArtifacts, Utf8PathBuf)>> {
    let base_path = contracts_path
        .parent()
        .ok_or_else(|| anyhow!("Failed to get parent for path = {}", &contracts_path))?;
    let artifacts = artifacts_for_package(contracts_path)?;
    let mut map = HashMap::new();

    for ref contract in artifacts.contracts {
        let name = contract.contract_name.clone();
        let contract_artifacts =
            StarknetContractArtifacts::from_scarb_contract_artifact(contract, base_path)?;

        let sierra_path = base_path.join(contract.artifacts.sierra.clone());

        map.insert(name.clone(), (contract_artifacts, sierra_path));
    }
    Ok(map)
}

fn compilation_unit_for_package<'a>(
    metadata: &'a Metadata,
    package: &PackageId,
) -> Result<&'a CompilationUnitMetadata> {
    metadata
        .compilation_units
        .iter()
        .filter(|unit| unit.package == *package)
        .min_by_key(|unit| match unit.target.kind.as_str() {
            name @ "starknet-contract" => (0, name),
            name @ "lib" => (1, name),
            name => (2, name),
        })
        .ok_or_else(|| anyhow!("Failed to find metadata for package = {package}"))
}

/// Get the target name for the given package
pub fn target_name_for_package(metadata: &Metadata, package: &PackageId) -> Result<String> {
    let compilation_unit = compilation_unit_for_package(metadata, package)?;
    Ok(compilation_unit.target.name.clone())
}

#[must_use]
pub fn target_dir_for_workspace(metadata: &Metadata) -> Utf8PathBuf {
    metadata
        .target_dir
        .clone()
        .unwrap_or_else(|| metadata.workspace.root.join("target"))
}

/// Get a name of the given package
pub fn name_for_package(metadata: &Metadata, package: &PackageId) -> Result<String> {
    let package = metadata
        .get_package(package)
        .ok_or_else(|| anyhow!("Failed to find metadata for package = {package}"))?;

    Ok(package.name.clone())
}

/// Checks if the specified package has version compatible with the specified requirement
pub fn package_matches_version_requirement(
    metadata: &Metadata,
    name: &str,
    version_req: &VersionReq,
) -> Result<bool> {
    let mut packages = metadata
        .packages
        .iter()
        .filter(|package| package.name == name);

    match (packages.next(), packages.next()) {
        (Some(package), None) => Ok(version_req.matches(&package.version)),
        (None, None) => Err(anyhow!("Package {name} is not present in dependencies.")),
        _ => Err(anyhow!("Package {name} is duplicated in dependencies")),
    }
}

/// collecting by name allow us to dedup targets
/// we do it because they use same sierra and we display them without distinction anyway
#[must_use]
pub fn test_targets_by_name(package: &PackageMetadata) -> HashMap<String, &TargetMetadata> {
    fn test_target_name(target: &TargetMetadata) -> String {
        // this is logic copied from scarb: https://github.com/software-mansion/scarb/blob/90ab01cb6deee48210affc2ec1dc94d540ab4aea/extensions/scarb-cairo-test/src/main.rs#L115
        target
            .params
            .get("group-id") // by unit tests grouping
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
            .unwrap_or(target.name.clone()) // else by integration test name
    }

    package
        .targets
        .iter()
        .filter(|target| target.kind == "test")
        .map(|target| (test_target_name(target), target))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::MetadataCommandExt;
    use assert_fs::fixture::{FileWriteStr, PathChild, PathCopy};
    use assert_fs::prelude::FileTouch;
    use assert_fs::TempDir;
    use camino::Utf8PathBuf;
    use indoc::{formatdoc, indoc};
    use std::str::FromStr;

    fn setup_package(package_name: &str) -> TempDir {
        let temp = TempDir::new().unwrap();
        temp.copy_from(
            format!("tests/data/{package_name}"),
            &["**/*.cairo", "**/*.toml"],
        )
        .unwrap();
        temp.copy_from("../../", &[".tool-versions"]).unwrap();

        let snforge_std_path = Utf8PathBuf::from_str("../../snforge_std")
            .unwrap()
            .canonicalize_utf8()
            .unwrap()
            .to_string()
            .replace('\\', "/");

        let manifest_path = temp.child("Scarb.toml");
        manifest_path
            .write_str(&formatdoc!(
                r#"
                [package]
                name = "{}"
                version = "0.1.0"

                [dependencies]
                starknet = "2.4.0"
                snforge_std = {{ path = "{}" }}

                [[target.starknet-contract]]

                [[tool.snforge.fork]]
                name = "FIRST_FORK_NAME"
                url = "http://some.rpc.url"
                block_id.number = "1"

                [[tool.snforge.fork]]
                name = "SECOND_FORK_NAME"
                url = "http://some.rpc.url"
                block_id.hash = "1"

                [[tool.snforge.fork]]
                name = "THIRD_FORK_NAME"
                url = "http://some.rpc.url"
                block_id.tag = "latest"
                "#,
                package_name,
                snforge_std_path
            ))
            .unwrap();

        temp
    }

    #[test]
    fn get_starknet_artifacts_path_for_standard_build() {
        let temp = setup_package("basic_package");

        ScarbCommand::new_with_stdio()
            .current_dir(temp.path())
            .arg("build")
            .run()
            .unwrap();

        let path = get_starknet_artifacts_path(
            &Utf8PathBuf::from_path_buf(temp.to_path_buf().join("target").join("dev")).unwrap(),
            "basic_package",
        )
        .unwrap();

        assert_eq!(
            path,
            ContractArtifactData {
                path: Utf8PathBuf::from_path_buf(
                    temp.path()
                        .join("target/dev/basic_package.starknet_artifacts.json")
                )
                .unwrap(),
                test_type: None
            }
        );
    }

    #[test]
    #[cfg_attr(not(feature = "scarb_2_8_3"), ignore)]
    fn get_starknet_artifacts_path_for_test_build() {
        let temp = setup_package("basic_package");

        ScarbCommand::new_with_stdio()
            .current_dir(temp.path())
            .arg("build")
            .arg("--test")
            .run()
            .unwrap();

        let metadata = ScarbCommand::metadata()
            .current_dir(temp.path())
            .run()
            .unwrap();

        let package = metadata
            .packages
            .iter()
            .find(|p| p.name == "basic_package")
            .unwrap();

        let path = get_starknet_artifacts_paths_from_test_targets(
            &Utf8PathBuf::from_path_buf(temp.join("target").join("dev")).unwrap(),
            &test_targets_by_name(package),
        );

        assert_eq!(
            path,
            vec![ContractArtifactData {
                path: Utf8PathBuf::from_path_buf(
                    temp.path()
                        .join("target/dev/basic_package_unittest.test.starknet_artifacts.json")
                )
                .unwrap(),
                test_type: Some("unit".to_string())
            }]
        );
    }

    #[test]
    #[cfg_attr(not(feature = "scarb_2_8_3"), ignore)]
    fn get_starknet_artifacts_path_for_test_build_when_integration_tests_exist() {
        let temp = setup_package("basic_package");
        let tests_dir = temp.join("tests");
        fs::create_dir(&tests_dir).unwrap();

        temp.child(tests_dir.join("test.cairo"))
            .write_str(indoc!(
                r"
                #[test]
                fn mock_test() {
                    assert!(true);
                }
            "
            ))
            .unwrap();

        ScarbCommand::new_with_stdio()
            .current_dir(temp.path())
            .arg("build")
            .arg("--test")
            .run()
            .unwrap();

        let metadata = ScarbCommand::metadata()
            .current_dir(temp.path())
            .run()
            .unwrap();

        let package = metadata
            .packages
            .iter()
            .find(|p| p.name == "basic_package")
            .unwrap();

        let path = get_starknet_artifacts_paths_from_test_targets(
            &Utf8PathBuf::from_path_buf(temp.to_path_buf().join("target").join("dev")).unwrap(),
            &test_targets_by_name(package),
        );

        assert_eq!(
            path,
            vec![
                ContractArtifactData {
                    path: Utf8PathBuf::from_path_buf(temp.path().join(
                        "target/dev/basic_package_integrationtest.test.starknet_artifacts.json"
                    ))
                    .unwrap(),
                    test_type: Some("integration".to_string())
                },
                ContractArtifactData {
                    path: Utf8PathBuf::from_path_buf(
                        temp.path()
                            .join("target/dev/basic_package_unittest.test.starknet_artifacts.json")
                    )
                    .unwrap(),
                    test_type: Some("unit".to_string())
                },
            ]
        );
    }

    #[test]
    fn package_matches_version_requirement_test() {
        let temp = setup_package("basic_package");

        let manifest_path = temp.child("Scarb.toml");
        manifest_path
            .write_str(&formatdoc!(
                r#"
                [package]
                name = "version_checker"
                version = "0.1.0"

                [[target.starknet-contract]]
                sierra = true

                [dependencies]
                starknet = "2.5.4"
                "#,
            ))
            .unwrap();

        let scarb_metadata = ScarbCommand::metadata()
            .inherit_stderr()
            .current_dir(temp.path())
            .run()
            .unwrap();

        assert!(package_matches_version_requirement(
            &scarb_metadata,
            "starknet",
            &VersionReq::parse("2.5").unwrap(),
        )
        .unwrap());

        assert!(package_matches_version_requirement(
            &scarb_metadata,
            "not_existing",
            &VersionReq::parse("2.5").unwrap(),
        )
        .is_err());

        assert!(!package_matches_version_requirement(
            &scarb_metadata,
            "starknet",
            &VersionReq::parse("2.8").unwrap(),
        )
        .unwrap());
    }

    #[test]
    fn get_starknet_artifacts_path_for_project_with_different_package_and_target_name() {
        let temp = setup_package("basic_package");

        let snforge_std_path = Utf8PathBuf::from_str("../../snforge_std")
            .unwrap()
            .canonicalize_utf8()
            .unwrap()
            .to_string()
            .replace('\\', "/");

        let scarb_path = temp.child("Scarb.toml");
        scarb_path
            .write_str(&formatdoc!(
                r#"
                [package]
                name = "basic_package"
                version = "0.1.0"

                [dependencies]
                starknet = "2.4.0"
                snforge_std = {{ path = "{}" }}

                [[target.starknet-contract]]
                name = "essa"
                sierra = true
                "#,
                snforge_std_path
            ))
            .unwrap();

        ScarbCommand::new_with_stdio()
            .current_dir(temp.path())
            .arg("build")
            .run()
            .unwrap();

        let path = get_starknet_artifacts_path(
            &Utf8PathBuf::from_path_buf(temp.to_path_buf().join("target").join("dev")).unwrap(),
            "essa",
        )
        .unwrap();

        assert_eq!(
            path,
            ContractArtifactData {
                path: Utf8PathBuf::from_path_buf(
                    temp.path().join("target/dev/essa.starknet_artifacts.json")
                )
                .unwrap(),
                test_type: None
            }
        );
    }

    #[test]
    fn get_starknet_artifacts_path_for_project_without_starknet_target() {
        let temp = setup_package("empty_lib");

        let manifest_path = temp.child("Scarb.toml");
        manifest_path
            .write_str(indoc!(
                r#"
            [package]
            name = "empty_lib"
            version = "0.1.0"
            "#,
            ))
            .unwrap();

        ScarbCommand::new_with_stdio()
            .current_dir(temp.path())
            .arg("build")
            .run()
            .unwrap();

        let path = get_starknet_artifacts_path(
            &Utf8PathBuf::from_path_buf(temp.to_path_buf().join("target").join("dev")).unwrap(),
            "empty_lib",
        );
        assert!(path.is_none());
    }

    #[test]
    fn get_starknet_artifacts_path_for_project_without_scarb_build() {
        let temp = setup_package("basic_package");

        let path = get_starknet_artifacts_path(
            &Utf8PathBuf::from_path_buf(temp.to_path_buf().join("target").join("dev")).unwrap(),
            "basic_package",
        );
        assert!(path.is_none());
    }

    #[test]
    fn parsing_starknet_artifacts() {
        let temp = setup_package("basic_package");

        ScarbCommand::new_with_stdio()
            .current_dir(temp.path())
            .arg("build")
            .run()
            .unwrap();

        let artifacts_path = temp
            .path()
            .join("target/dev/basic_package.starknet_artifacts.json");
        let artifacts_path = Utf8PathBuf::from_path_buf(artifacts_path).unwrap();

        let artifacts = artifacts_for_package(&artifacts_path).unwrap();

        assert!(!artifacts.contracts.is_empty());
    }

    #[test]
    fn parsing_starknet_artifacts_on_invalid_file() {
        let temp = TempDir::new().unwrap();
        temp.copy_from("../../", &[".tool-versions"]).unwrap();
        let path = temp.child("wrong.json");
        path.touch().unwrap();
        path.write_str("\"aa\": {}").unwrap();
        let artifacts_path = Utf8PathBuf::from_path_buf(path.to_path_buf()).unwrap();

        let result = artifacts_for_package(&artifacts_path);
        let err = result.unwrap_err();

        assert!(err.to_string().contains(&format!("Failed to parse {artifacts_path:?} contents. Make sure you have enabled sierra code generation in Scarb.toml")));
    }

    #[test]
    fn get_contracts() {
        let temp = setup_package("basic_package");

        ScarbCommand::new_with_stdio()
            .current_dir(temp.path())
            .arg("build")
            .run()
            .unwrap();

        let metadata = ScarbCommand::metadata()
            .inherit_stderr()
            .manifest_path(temp.join("Scarb.toml"))
            .run()
            .unwrap();

        let target_dir = target_dir_for_workspace(&metadata).join("dev");
        let package = metadata.packages.first().unwrap();

        let contracts = get_contracts_artifacts_and_source_sierra_paths(
            &metadata,
            target_dir.as_path(),
            package,
            false,
        )
        .unwrap();

        assert!(contracts.contains_key("ERC20"));
        assert!(contracts.contains_key("HelloStarknet"));

        let sierra_contents_erc20 =
            fs::read_to_string(temp.join("target/dev/basic_package_ERC20.contract_class.json"))
                .unwrap();

        let contract = contracts.get("ERC20").unwrap();
        assert_eq!(&sierra_contents_erc20, &contract.0.sierra);
        assert!(!contract.0.casm.is_empty());

        let sierra_contents_erc20 = fs::read_to_string(
            temp.join("target/dev/basic_package_HelloStarknet.contract_class.json"),
        )
        .unwrap();
        let contract = contracts.get("HelloStarknet").unwrap();
        assert_eq!(&sierra_contents_erc20, &contract.0.sierra);
        assert!(!contract.0.casm.is_empty());
    }

    #[test]
    fn get_name_for_package() {
        let temp = setup_package("basic_package");
        let scarb_metadata = ScarbCommand::metadata()
            .inherit_stderr()
            .current_dir(temp.path())
            .run()
            .unwrap();

        let package_name =
            name_for_package(&scarb_metadata, &scarb_metadata.workspace.members[0]).unwrap();

        assert_eq!(&package_name, "basic_package");
    }

    #[test]
    fn get_target_name_for_package() {
        let temp = setup_package("basic_package");
        let scarb_metadata = ScarbCommand::metadata()
            .inherit_stderr()
            .current_dir(temp.path())
            .run()
            .unwrap();

        let target_name =
            target_name_for_package(&scarb_metadata, &scarb_metadata.workspace.members[0]).unwrap();

        assert_eq!(target_name, "basic_package");
    }
}
