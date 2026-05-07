// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {ISP1Verifier} from "@sp1-contracts/ISP1Verifier.sol";

/// @title Rollup
/// @notice Settlement contract for an SP1-proved zk-rollup. Holds the canonical
///         L2 state root and advances it whenever a sequencer submits a batch
///         together with a proof produced by the rollup's pinned SP1 program.
///
/// @dev    The SP1 program commits public values as
///         `(bytes32 prevRoot, bytes32 newRoot, bytes32 batchHash)` via
///         `alloy_sol_types::sol!`, so the host abi-encodes them and we can
///         abi-decode here cheaply.
contract Rollup {
    /// @notice Verification key of the SP1 program permitted to advance the root.
    bytes32 public immutable programVKey;

    /// @notice The SP1 verifier (mock or real) the on-chain proof check delegates to.
    address public immutable verifier;

    /// @notice Current canonical L2 state root.
    bytes32 public stateRoot;

    /// @notice Number of settled batches; useful for indexing and as a sanity counter.
    uint64 public batchCount;

    event BatchSettled(uint64 indexed batchNumber, bytes32 prevRoot, bytes32 newRoot, bytes32 batchHash);

    error ZeroVerifier();
    error ZeroProgramVKey();
    error StaleStateRoot(bytes32 expected, bytes32 actual);
    error EmptyNewRoot();

    constructor(address _verifier, bytes32 _programVKey, bytes32 _genesisRoot) {
        if (_verifier == address(0)) revert ZeroVerifier();
        if (_programVKey == bytes32(0)) revert ZeroProgramVKey();
        verifier = _verifier;
        programVKey = _programVKey;
        stateRoot = _genesisRoot;
    }

    /// @notice Verify a batch proof and advance the state root.
    /// @param  publicValues abi.encode(prevRoot, newRoot, batchHash) — exactly what the
    ///         SP1 program committed via `commit_slice`.
    /// @param  proofBytes The SP1 proof. Empty (0 bytes) when paired with `SP1MockVerifier`.
    function submitBatch(bytes calldata publicValues, bytes calldata proofBytes) external {
        ISP1Verifier(verifier).verifyProof(programVKey, publicValues, proofBytes);

        (bytes32 prevRoot, bytes32 newRoot, bytes32 batchHash) =
            abi.decode(publicValues, (bytes32, bytes32, bytes32));

        if (prevRoot != stateRoot) revert StaleStateRoot(stateRoot, prevRoot);
        if (newRoot == bytes32(0)) revert EmptyNewRoot();

        stateRoot = newRoot;
        unchecked {
            batchCount += 1;
        }

        emit BatchSettled(batchCount, prevRoot, newRoot, batchHash);
    }
}
