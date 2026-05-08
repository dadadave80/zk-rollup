// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {Test} from "forge-std/Test.sol";
import {SP1MockVerifier} from "@sp1-contracts/SP1MockVerifier.sol";
import {Rollup} from "../src/Rollup.sol";

contract RollupTest is Test {
    SP1MockVerifier internal verifier;
    Rollup internal rollup;

    bytes32 internal constant VKEY = bytes32(uint256(0xCAFE));
    bytes32 internal constant GENESIS_ROOT = bytes32(uint256(0x1111));

    event BatchSettled(uint64 indexed batchNumber, bytes32 prevRoot, bytes32 newRoot, bytes32 batchHash);

    function setUp() public {
        verifier = new SP1MockVerifier();
        rollup = new Rollup(address(verifier), VKEY, GENESIS_ROOT);
    }

    function _publicValues(bytes32 prev, bytes32 next, bytes32 batchHash) internal pure returns (bytes memory) {
        return abi.encode(prev, next, batchHash);
    }

    function test_ConstructorStoresImmutables() public view {
        assertEq(rollup.verifier(), address(verifier));
        assertEq(rollup.programVKey(), VKEY);
        assertEq(rollup.stateRoot(), GENESIS_ROOT);
        assertEq(rollup.batchCount(), 0);
    }

    function test_RevertWhen_VerifierIsZero() public {
        vm.expectRevert(Rollup.ZeroVerifier.selector);
        new Rollup(address(0), VKEY, GENESIS_ROOT);
    }

    function test_RevertWhen_ProgramVKeyIsZero() public {
        vm.expectRevert(Rollup.ZeroProgramVKey.selector);
        new Rollup(address(verifier), bytes32(0), GENESIS_ROOT);
    }

    function test_SubmitBatch_AdvancesRootAndEmits() public {
        bytes32 newRoot = bytes32(uint256(0x2222));
        bytes32 batchHash = bytes32(uint256(0xBBBB));
        bytes memory pv = _publicValues(GENESIS_ROOT, newRoot, batchHash);

        vm.expectEmit(true, true, true, true);
        emit BatchSettled(1, GENESIS_ROOT, newRoot, batchHash);
        rollup.submitBatch(pv, "");

        assertEq(rollup.stateRoot(), newRoot);
        assertEq(rollup.batchCount(), 1);
    }

    function test_RevertWhen_PrevRootIsStale() public {
        bytes32 wrongPrev = bytes32(uint256(0xDEAD));
        bytes32 newRoot = bytes32(uint256(0x2222));
        bytes memory pv = _publicValues(wrongPrev, newRoot, bytes32(uint256(0xBBBB)));

        vm.expectRevert(abi.encodeWithSelector(Rollup.StaleStateRoot.selector, GENESIS_ROOT, wrongPrev));
        rollup.submitBatch(pv, "");
        assertEq(rollup.stateRoot(), GENESIS_ROOT, "state root must not have moved");
    }

    function test_RevertWhen_NewRootIsZero() public {
        bytes memory pv = _publicValues(GENESIS_ROOT, bytes32(0), bytes32(uint256(0xBBBB)));
        vm.expectRevert(Rollup.EmptyNewRoot.selector);
        rollup.submitBatch(pv, "");
    }

    function test_RevertWhen_MockVerifierGetsNonEmptyProof() public {
        bytes32 newRoot = bytes32(uint256(0x2222));
        bytes memory pv = _publicValues(GENESIS_ROOT, newRoot, bytes32(uint256(0xBBBB)));
        // SP1MockVerifier asserts proofBytes.length == 0; non-empty triggers a panic.
        vm.expectRevert();
        rollup.submitBatch(pv, hex"deadbeef");
    }

    function test_SequentialBatches_ChainCorrectly() public {
        bytes32 r0 = GENESIS_ROOT;
        bytes32 r1 = bytes32(uint256(0x2222));
        bytes32 r2 = bytes32(uint256(0x3333));
        bytes32 r3 = bytes32(uint256(0x4444));

        rollup.submitBatch(_publicValues(r0, r1, bytes32(uint256(0xB1))), "");
        rollup.submitBatch(_publicValues(r1, r2, bytes32(uint256(0xB2))), "");
        rollup.submitBatch(_publicValues(r2, r3, bytes32(uint256(0xB3))), "");

        assertEq(rollup.stateRoot(), r3);
        assertEq(rollup.batchCount(), 3);
    }

    function test_RevertWhen_SecondBatchUsesOldPrev() public {
        bytes32 r1 = bytes32(uint256(0x2222));
        bytes32 r2 = bytes32(uint256(0x3333));

        rollup.submitBatch(_publicValues(GENESIS_ROOT, r1, bytes32(uint256(0xB1))), "");

        // Try to submit a second batch that still claims GENESIS_ROOT as prev — must revert.
        bytes memory pv = _publicValues(GENESIS_ROOT, r2, bytes32(uint256(0xB2)));
        vm.expectRevert(abi.encodeWithSelector(Rollup.StaleStateRoot.selector, r1, GENESIS_ROOT));
        rollup.submitBatch(pv, "");
    }
}
