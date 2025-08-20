// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/**
 * Test contract to emulate LR provider 
 * Allows fixed-price ETH <-> ERC20 swap, with slippage
 *
 * - Anyone can top up contract ETH via depositEth() or send tokens via depositTokens()
 *   (requires prior approve() to this contract).
 * - Owner can set the price in "tokens per 1 ETH" assuming tokens expressed in smallest units.
 *   Example: price = 2e18 means 1 ETH swaps for 2.0 tokens (assuming token has 18 decimals).
 * - Users can swap ETH->token and token->ETH at the set price, subject to contract liquidity.
 */

interface IERC20Metadata is IERC20 {
    function decimals() external view returns (uint8);
}



/* ----------------- Contract ----------------- */
contract EthTokenFixedPriceSwap {
    address public immutable owner;
    IERC20 public immutable token;
    uint8  public immutable tokenDecimals;

    // Price as tokens per 1 ETH, in tokem smallest units.
    // Example: if token decimals is 8 then price of 1e8 => 1 token per ETH; 2e8 => 2 tokens per ETH.
    uint256 public tokensPerEth;

    event PriceUpdated(uint256 oldPrice, uint256 newPrice);
    event EthDeposited(address indexed from, uint256 amount);
    event TokenDeposited(address indexed from, uint256 amount);
    event SwappedEthForTokens(address indexed user, uint256 ethIn, uint256 tokensOut);
    event SwappedTokensForEth(address indexed user, uint256 tokensIn, uint256 ethOut);
    event Withdrawn(address indexed to, uint256 ethAmount, uint256 tokenAmount);

    modifier onlyOwner() {
        require(msg.sender == owner, "NOT_OWNER");
        _;
    }

    constructor(address tokenAddress, uint256 initialTokensPerEth) {
        require(tokenAddress != address(0), "TOKEN_ZERO");
        require(initialTokensPerEth > 0, "PRICE_ZERO");
        owner = msg.sender;
        token = IERC20(tokenAddress);
        // Try to read decimals if available
        uint8 decs = 18;
        try IERC20Metadata(tokenAddress).decimals() returns (uint8 d) {
            decs = d;
        } catch { /* default 18 if not implemented */ }
        tokenDecimals = decs;

        tokensPerEth = initialTokensPerEth;
    }

    /* ----------------- Owner controls ----------------- */

    /// @notice Set price in tokens per 1 ETH (18-dec fixed point).
    function setTokensPerEth(uint256 newPrice) external onlyOwner {
        require(newPrice > 0, "PRICE_ZERO");
        emit PriceUpdated(tokensPerEth, newPrice);
        tokensPerEth = newPrice;
    }

    /// @notice Owner can withdraw ETH and/or tokens (for maintenance or closing).
    function ownerWithdraw(address payable to, uint256 ethAmount, uint256 tokenAmount) external onlyOwner {
        if (ethAmount > 0) {
            require(address(this).balance >= ethAmount, "INSUFFICIENT_ETH");
            (bool ok, ) = to.call{value: ethAmount}("");
            require(ok, "ETH_WITHDRAW_FAILED");
        }
        if (tokenAmount > 0) {
            require(token.balanceOf(address(this)) >= tokenAmount, "INSUFFICIENT_TOKEN");
            token.transfer(to, tokenAmount);
        }
        emit Withdrawn(to, ethAmount, tokenAmount);
    }

    /* ----------------- Top-ups (anyone) ----------------- */

    /// @notice Deposit ETH liquidity.
    function depositEth() external payable {
        require(msg.value > 0, "NO_ETH");
        emit EthDeposited(msg.sender, msg.value);
    }

    /// @notice Deposit token liquidity (requires prior approve to this contract).
    function depositTokens(uint256 amount) external {
        require(amount > 0, "NO_TOKENS");
        token.transferFrom(msg.sender, address(this), amount);
        emit TokenDeposited(msg.sender, amount);
    }

    /* ----------------- Swaps ----------------- */

    /// @notice Swap sent ETH for tokens at current price.
    /// @dev tokensOut = msg.value * tokensPerEth / 1e18, scaled to token decimals.
    /// slippage is in 1/1000 units
    function swapEthForTokens(uint256 slippage) external payable {
        require(msg.value > 0, "NO_ETH");
        require(slippage <= 1000, "BAD_SLIPPAGE");
        // Compute tokens out with 18-dec fixed point price. Token may not be 18 decimals; scale accordingly.
        uint256 tokensOut = _ethToToken(msg.value, slippage);
        require(token.balanceOf(address(this)) >= tokensOut, "INSUFFICIENT_TOKEN_LIQ");

        token.transfer(msg.sender, tokensOut);
        emit SwappedEthForTokens(msg.sender, msg.value, tokensOut);
        // ETH stays in contract as liquidity.
    }

    /// @notice Swap tokens for ETH at current price (send tokens first via approve()).
    /// @dev ethOut = tokensIn * 1e18 / tokensPerEth, accounting for token decimals.
    function swapTokensForEth(uint256 tokensIn, uint256 slippage) external {
        require(tokensIn > 0, "NO_TOKENS");
        require(slippage <= 1000, "BAD_SLIPPAGE");

        // Pull tokens in
        token.transferFrom(msg.sender, address(this), tokensIn);

        uint256 ethOut = _tokenToEth(tokensIn, slippage);
        require(address(this).balance >= ethOut, "INSUFFICIENT_ETH_LIQ");

        (bool ok, ) = payable(msg.sender).call{value: ethOut}("");
        require(ok, "ETH_SEND_FAILED");

        emit SwappedTokensForEth(msg.sender, tokensIn, ethOut);
    }

    /* ----------------- Helpers ----------------- */

    // Convert ETH amount (wei) to token amount considering price and token decimals.
    function _ethToToken(uint256 ethAmountWei, uint256 slippage) internal view returns (uint256) {
        // tokensPerEth has 18 decimals. ethAmountWei has 18 decimals (wei).
        // Base tokensOut in 18-dec units:
        uint256 tokensPerEthWithSlippage = tokensPerEth - tokensPerEth * slippage / 1000;
        uint256 tokensOut = (ethAmountWei * tokensPerEthWithSlippage) / 1e18;

        //if (tokenDecimals == 18) return tokensOut18;
        //if (tokenDecimals > 18) return tokensOut18 * (10 ** (tokenDecimals - 18));
        // tokenDecimals < 18
        //return tokensOut18 / (10 ** (18 - tokenDecimals));
        return tokensOut;
    }

    // Convert token amount to ETH amount (wei) considering price and token decimals.
    function _tokenToEth(uint256 tokenAmount, uint256 slippage) internal view returns (uint256) {
        // Normalize tokenAmount to 18 decimals:
        uint256 tokenAmount18;
        if (tokenDecimals == 18) tokenAmount18 = tokenAmount;
        else if (tokenDecimals > 18) tokenAmount18 = tokenAmount / (10 ** (tokenDecimals - 18));
        else tokenAmount18 = tokenAmount * (10 ** (18 - tokenDecimals));

        uint256 tokensPerEthWithSlippage = tokensPerEth + tokensPerEth * slippage / 1000;
        return (tokenAmount18 * 1e18) / tokensPerEthWithSlippage;
    }

    // Allow receiving plain ETH (treated as deposit).
    receive() external payable {
        require(msg.value > 0, "NO_ETH");
        emit EthDeposited(msg.sender, msg.value);
    }
}
