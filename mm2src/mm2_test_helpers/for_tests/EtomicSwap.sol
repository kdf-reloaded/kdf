// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

// Clean-room re-implementation of the v1 EtomicSwap HTLC contract, authored
// solely from the public `SWAP_CONTRACT_ABI` embedded in this crate
// (`mm2src/coins/eth/eth_types.rs`) plus the standard hash-time-locked-contract
// pattern. It is used ONLY as a local test fixture: the eth swap integration
// tests deploy it to a throwaway `geth --dev` chain to exercise the KDF-side
// payment/refund code paths end-to-end.
//
// The function signatures (and therefore the 4-byte selectors) match the ABI
// exactly so KDF's `ethabi`-encoded calls are accepted. The payment commitment
// hash is computed and verified entirely inside this contract, so it only has
// to be internally self-consistent — KDF never computes it, it only passes the
// arguments through.

interface IERC20 {
    function transfer(address to, uint256 value) external returns (bool);
    function transferFrom(address from, address to, uint256 value) external returns (bool);
}

contract EtomicSwap {
    enum PaymentState {
        Uninitialized,
        PaymentSent,
        ReceivedSpent,
        SenderRefunded
    }

    struct Payment {
        bytes20 paymentHash;
        uint64 lockTime;
        PaymentState state;
    }

    mapping (bytes32 => Payment) public payments;

    event PaymentSent(bytes32 id);
    event ReceiverSpent(bytes32 id, bytes32 secret);
    event SenderRefunded(bytes32 id);

    constructor() {}

    function ethPayment(
        bytes32 _id,
        address _receiver,
        bytes20 _secretHash,
        uint64 _lockTime
    ) external payable {
        require(_receiver != address(0), "receiver is zero");
        require(msg.value > 0, "no value");
        require(payments[_id].state == PaymentState.Uninitialized, "id in use");

        bytes20 paymentHash = ripemd160(
            abi.encodePacked(_receiver, msg.sender, _secretHash, address(0), msg.value)
        );
        payments[_id] = Payment(paymentHash, _lockTime, PaymentState.PaymentSent);
        emit PaymentSent(_id);
    }

    function erc20Payment(
        bytes32 _id,
        uint256 _amount,
        address _tokenAddress,
        address _receiver,
        bytes20 _secretHash,
        uint64 _lockTime
    ) external payable {
        require(_receiver != address(0), "receiver is zero");
        require(_amount > 0, "no amount");
        require(_tokenAddress != address(0), "token is zero");
        require(payments[_id].state == PaymentState.Uninitialized, "id in use");

        bytes20 paymentHash = ripemd160(
            abi.encodePacked(_receiver, msg.sender, _secretHash, _tokenAddress, _amount)
        );
        payments[_id] = Payment(paymentHash, _lockTime, PaymentState.PaymentSent);
        require(
            IERC20(_tokenAddress).transferFrom(msg.sender, address(this), _amount),
            "transferFrom failed"
        );
        emit PaymentSent(_id);
    }

    function receiverSpend(
        bytes32 _id,
        uint256 _amount,
        bytes32 _secret,
        address _tokenAddress,
        address _sender
    ) external {
        require(payments[_id].state == PaymentState.PaymentSent, "not sent");

        bytes20 secretHash = ripemd160(abi.encodePacked(sha256(abi.encodePacked(_secret))));
        bytes20 paymentHash = ripemd160(
            abi.encodePacked(msg.sender, _sender, secretHash, _tokenAddress, _amount)
        );
        require(paymentHash == payments[_id].paymentHash, "hash mismatch");

        payments[_id].state = PaymentState.ReceivedSpent;
        if (_tokenAddress == address(0)) {
            payable(msg.sender).transfer(_amount);
        } else {
            require(IERC20(_tokenAddress).transfer(msg.sender, _amount), "transfer failed");
        }
        emit ReceiverSpent(_id, _secret);
    }

    function senderRefund(
        bytes32 _id,
        uint256 _amount,
        bytes20 _paymentHash,
        address _tokenAddress,
        address _receiver
    ) external {
        require(payments[_id].state == PaymentState.PaymentSent, "not sent");

        bytes20 paymentHash = ripemd160(
            abi.encodePacked(_receiver, msg.sender, _paymentHash, _tokenAddress, _amount)
        );
        require(paymentHash == payments[_id].paymentHash, "hash mismatch");
        require(block.timestamp >= payments[_id].lockTime, "not expired");

        payments[_id].state = PaymentState.SenderRefunded;
        if (_tokenAddress == address(0)) {
            payable(msg.sender).transfer(_amount);
        } else {
            require(IERC20(_tokenAddress).transfer(msg.sender, _amount), "transfer failed");
        }
        emit SenderRefunded(_id);
    }
}
