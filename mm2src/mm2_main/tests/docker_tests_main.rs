#![cfg(feature = "run-docker-tests")]
#![feature(async_closure)]
#![feature(custom_test_frameworks)]
#![feature(test)]
#![test_runner(docker_tests_runner)]
#![feature(drain_filter)]
#![feature(hash_raw_entry)]
#![cfg(not(target_arch = "wasm32"))]
#![feature(local_key_cell_methods)] // for setting global vars in tests

#[cfg(test)]
#[macro_use]
extern crate common;
#[cfg(test)]
#[macro_use]
extern crate gstuff;
#[cfg(test)]
#[macro_use]
extern crate lazy_static;
#[cfg(test)]
#[macro_use]
extern crate serde_json;
#[cfg(test)] extern crate ser_error_derive;
#[cfg(test)] extern crate test;

use common::custom_futures::timeout::FutureTimerExt;
use std::env;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use test::{test_main, StaticBenchFn, StaticTestFn, TestDescAndFn};
use testcontainers::clients::Cli;

mod docker_tests;
use docker_tests::docker_tests_common::*;
use docker_tests::qrc20_tests::{qtum_docker_node, QtumDockerOps, QTUM_REGTEST_DOCKER_IMAGE_WITH_TAG};

#[allow(dead_code)] mod integration_tests_common;

// AP: custom test runner is intended to initialize the required environment (e.g. coin daemons in the docker containers)
// and then gracefully clear it by dropping the RAII docker container handlers
// I've tried to use static for such singleton initialization but it turned out that despite
// rustc allows to use Drop as static the drop fn won't ever be called
// NB: https://github.com/rust-lang/rfcs/issues/1111
// the only preparation step required is Zcash params files downloading:
// Windows - https://github.com/KomodoPlatform/komodo/blob/master/zcutil/fetch-params.bat
// Linux and MacOS - https://github.com/KomodoPlatform/komodo/blob/master/zcutil/fetch-params.sh
pub fn docker_tests_runner(tests: &[&TestDescAndFn]) {
    // pretty_env_logger::try_init();
    let docker = Cli::default();
    let mut containers = vec![];
    // skip Docker containers initialization if we are intended to run test_mm_start only
    if env::var("_MM2_TEST_CONF").is_err() {
        const IMAGES: &[&str] = &[
            UTXO_ASSET_DOCKER_IMAGE_WITH_TAG,
            QTUM_REGTEST_DOCKER_IMAGE_WITH_TAG,
            ZOMBIE_ASSET_DOCKER_IMAGE_WITH_TAG,
            GETH_DOCKER_IMAGE_WITH_TAG,
            NUCLEUS_IMAGE,
            ATOM_IMAGE_WITH_TAG,
            IBC_RELAYER_IMAGE_WITH_TAG,
        ];

        for image in IMAGES {
            pull_docker_image(image);
            remove_docker_containers(image);
        }

        let runtime_dir = prepare_runtime_dir().unwrap();

        let nucleus_node = nucleus_node(&docker, runtime_dir.clone());
        let atom_node = atom_node(&docker, runtime_dir.clone());
        let ibc_relayer_node = ibc_relayer_node(&docker, runtime_dir);
        let utxo_node = utxo_asset_docker_node(&docker, "MYCOIN", 7000);
        let utxo_node1 = utxo_asset_docker_node(&docker, "MYCOIN1", 8000);
        let qtum_node = qtum_docker_node(&docker, 9000);
        let for_slp_node = utxo_asset_docker_node(&docker, "FORSLP", 10000);
        let geth_node = geth_docker_node(&docker, "ETH", 8545);
        let zombie_node = zombie_asset_docker_node(&docker, 7090);

        let utxo_ops = UtxoAssetDockerOps::from_ticker("MYCOIN");
        let utxo_ops1 = UtxoAssetDockerOps::from_ticker("MYCOIN1");
        let qtum_ops = QtumDockerOps::new();
        let for_slp_ops = BchDockerOps::from_ticker("FORSLP");
        let zombie_ops = ZCoinAssetDockerOps::new();

        zombie_ops.wait_ready(4);
        qtum_ops.wait_ready(2);
        qtum_ops.initialize_contracts();
        for_slp_ops.wait_ready(4);
        for_slp_ops.initialize_slp();
        utxo_ops.wait_ready(4);
        utxo_ops1.wait_ready(4);

        wait_for_geth_node_ready();
        init_geth_node();
        prepare_ibc_channels(ibc_relayer_node.container.id());

        thread::sleep(Duration::from_secs(12));
        wait_until_relayer_container_is_ready(ibc_relayer_node.container.id());

        containers.push(utxo_node);
        containers.push(utxo_node1);
        containers.push(qtum_node);
        containers.push(for_slp_node);
        containers.push(zombie_node);
        containers.push(geth_node);
        containers.push(nucleus_node);
        containers.push(atom_node);
        containers.push(ibc_relayer_node);
    }
    // detect if docker is installed
    // skip the tests that use docker if not installed
    let owned_tests: Vec<_> = tests
        .iter()
        .map(|t| match t.testfn {
            StaticTestFn(f) => TestDescAndFn {
                testfn: StaticTestFn(f),
                desc: t.desc.clone(),
            },
            StaticBenchFn(f) => TestDescAndFn {
                testfn: StaticBenchFn(f),
                desc: t.desc.clone(),
            },
            _ => panic!("non-static tests passed to lp_coins test runner"),
        })
        .collect();
    let args: Vec<String> = env::args().collect();
    test_main(&args, owned_tests, None);
}

fn wait_for_geth_node_ready() {
    let mut attempts = 0;
    loop {
        if attempts >= 5 {
            panic!("Failed to connect to Geth node after several attempts.");
        }
        match block_on(GETH_WEB3.eth().block_number().timeout(Duration::from_secs(6))) {
            Ok(Ok(block_number)) => {
                log!("Geth node is ready, latest block number: {:?}", block_number);
                break;
            },
            Ok(Err(e)) => {
                log!("Failed to connect to Geth node: {:?}, retrying...", e);
            },
            Err(_) => {
                log!("Connection to Geth node timed out, retrying...");
            },
        }
        attempts += 1;
        thread::sleep(Duration::from_secs(1));
    }
}

fn pull_docker_image(name: &str) {
    Command::new("docker")
        .arg("pull")
        .arg(name)
        .status()
        .expect("Failed to execute docker command");
}

fn remove_docker_containers(name: &str) {
    let stdout = Command::new("docker")
        .arg("ps")
        .arg("-f")
        .arg(format!("ancestor={}", name))
        .arg("-q")
        .output()
        .expect("Failed to execute docker command");

    let reader = BufReader::new(stdout.stdout.as_slice());
    let ids: Vec<_> = reader.lines().map(|line| line.unwrap()).collect();
    if !ids.is_empty() {
        Command::new("docker")
            .arg("rm")
            .arg("-f")
            .args(ids)
            .status()
            .expect("Failed to execute docker command");
    }
}

fn prepare_runtime_dir() -> std::io::Result<PathBuf> {
    let project_root = {
        let mut current_dir = std::env::current_dir().unwrap();
        current_dir.pop();
        current_dir.pop();
        current_dir
    };

    let containers_state_dir = project_root.join(".docker/container-state");
    assert!(containers_state_dir.exists());
    let containers_runtime_dir = project_root.join(".docker/container-runtime");

    // Remove runtime directory if it exists to copy containers files to a clean directory
    if containers_runtime_dir.exists() {
        std::fs::remove_dir_all(&containers_runtime_dir).unwrap();
    }

    // Copy container files to runtime directory
    mm2_io::fs::copy_dir_all(&containers_state_dir, &containers_runtime_dir).unwrap();

    Ok(containers_runtime_dir)
}
