"""
Test script for OrderBook Manager
"""

import asyncio
import logging
from orderbook_manager import OrderBookManager

async def test_orderbook():
    """Test the orderbook manager functionality"""
    
    # Configure logging
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s - %(levelname)s - %(message)s'
    )
    
    logger = logging.getLogger(__name__)
    logger.info("Starting OrderBook test...")
    
    # Create orderbook manager
    ob_manager = OrderBookManager(symbol="BTCUSDT")
    
    try:
        # Start the manager
        await ob_manager.start()
        
        # Wait for initial sync
        await asyncio.sleep(2)
        
        # Monitor for a short while
        for i in range(10):  # Run for 10 iterations
            await asyncio.sleep(1)
            
            # Get features
            features = ob_manager.get_orderbook_features()
            status = ob_manager.get_status()
            
            logger.info(f"=== Update {i+1} ===")
            logger.info(f"Mid Price: {features.get('mid_price', 'N/A'):.2f}")
            logger.info(f"Spread (bps): {features.get('spread_bps', 'N/A'):.1f}")
            logger.info(f"OBI Levels - 1: {features.get('obi_1', 'N/A'):.3f}, 5: {features.get('obi_5', 'N/A'):.3f}, 10: {features.get('obi_10', 'N/A'):.3f}")
            logger.info(f"Depth Imbalance (10-level): {features.get('depth_imbalance_10', 'N/A'):.3f}")
            logger.info(f"Status: Sync={status['is_synchronized']}, Updates={status['update_count']}, Latency={status['update_latency_ms']:.2f}ms")
            
            # Validate orderbook
            is_valid = ob_manager.validate_orderbook()
            logger.info(f"Orderbook Valid: {is_valid}")
            
    except KeyboardInterrupt:
        logger.info("Test interrupted by user")
    except Exception as e:
        logger.error(f"Test error: {e}")
    finally:
        await ob_manager.stop()
        logger.info("Test completed")

if __name__ == "__main__":
    asyncio.run(test_orderbook())