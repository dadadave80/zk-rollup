// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {Script, console} from "forge-std/Script.sol";
import {SP1MockVerifier} from "@sp1-contracts/SP1MockVerifier.sol";
import {Rollup} from "../src/Rollup.sol";

/// @notice Deploy script for Rollup. Driven entirely by env so the same script works
///         on local anvil and on Sepolia. The Bun orchestrator sets the env before
///         shelling out to `forge script`.
///
/// Required env:
///   PROGRAM_VKEY     — bytes32 verification key from `cargo run -p sp1-script --bin vkey`
///   GENESIS_ROOT     — bytes32 initial L2 state root (sequencer prints this on startup)
///
/// Optional env:
///   PROOF_MODE       — "mock" (default) or "groth16"
///   SP1_VERIFIER     — explicit verifier address. If unset:
///                        mock     → deploy a fresh SP1MockVerifier
///                        groth16  → revert (require an explicit address; on Sepolia
///                                  the canonical gateway is
///                                  0x3B6041173B80E77f038f3F2C0f9744f04837185e but
///                                  callers should pass it in to keep the script
///                                  agnostic of the chain)
contract Deploy is Script {
    function run() external returns (Rollup rollup, address verifier) {
        bytes32 programVKey = vm.envBytes32("PROGRAM_VKEY");
        bytes32 genesisRoot = vm.envBytes32("GENESIS_ROOT");
        string memory proofMode = vm.envOr("PROOF_MODE", string("mock"));
        address presetVerifier = vm.envOr("SP1_VERIFIER", address(0));

        vm.startBroadcast();

        if (presetVerifier != address(0)) {
            verifier = presetVerifier;
            console.log("Using preset SP1 verifier at", verifier);
        } else if (_isMock(proofMode)) {
            verifier = address(new SP1MockVerifier());
            console.log("Deployed SP1MockVerifier at", verifier);
        } else {
            revert(
                "groth16 mode requires SP1_VERIFIER env: pass the canonical SP1VerifierGateway address for the target chain"
            );
        }

        rollup = new Rollup(verifier, programVKey, genesisRoot);

        vm.stopBroadcast();

        console.log("Rollup deployed at      ", address(rollup));
        console.log("  programVKey           ");
        console.logBytes32(programVKey);
        console.log("  genesisRoot           ");
        console.logBytes32(genesisRoot);
        console.log("  verifier              ", verifier);
        console.log("  proofMode             ", proofMode);
    }

    function _isMock(string memory mode) internal pure returns (bool) {
        return keccak256(bytes(mode)) == keccak256(bytes("mock"));
    }
}
