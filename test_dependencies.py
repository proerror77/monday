#!/usr/bin/env python3
"""
Simple test script to verify all dependencies are working
"""

def test_imports():
    """Test if all required modules can be imported"""
    try:
        import asyncio
        print("✓ asyncio imported successfully")
        
        import websockets
        print("✓ websockets imported successfully")
        
        import json
        print("✓ json imported successfully")
        
        import requests
        print("✓ requests imported successfully")
        
        import time
        print("✓ time imported successfully")
        
        import logging
        print("✓ logging imported successfully")
        
        import hmac
        print("✓ hmac imported successfully")
        
        import hashlib
        print("✓ hashlib imported successfully")
        
        import base64
        print("✓ base64 imported successfully")
        
        print("\n✅ All dependencies are available!")
        return True
        
    except ImportError as e:
        print(f"\n❌ Import error: {e}")
        print("Please install missing dependencies:")
        print("pip install --user websockets requests")
        return False

if __name__ == "__main__":
    print("Testing dependencies for BitgetHFTBot...")
    test_imports()