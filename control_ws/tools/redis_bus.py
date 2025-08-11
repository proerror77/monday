from typing import Optional, Dict, Any, Iterable


class RedisBusTool:
    """Lightweight Redis pub/sub wrapper used by control plane agents.

    This is a stub to avoid hard dependency. Replace with redis.StrictRedis in runtime.
    """

    def __init__(self, redis_url: str):
        self.redis_url = redis_url
        self._client = None  # lazy

    def _ensure(self):
        if self._client is None:
            try:
                import redis  # type: ignore

                self._client = redis.Redis.from_url(self.redis_url, decode_responses=True)
            except Exception:
                self._client = None

    def publish(self, channel: str, message: Dict[str, Any]) -> None:
        self._ensure()
        if not self._client:
            return
        import json

        self._client.publish(channel, json.dumps(message))

    def subscribe(self, channels: Iterable[str]):
        self._ensure()
        if not self._client:
            return None
        ps = self._client.pubsub(ignore_subscribe_messages=True)
        for ch in channels:
            ps.subscribe(ch)
        return ps

    def next_message(self, pubsub, timeout: float = 1.0):
        if not pubsub:
            return None
        return pubsub.get_message(timeout=timeout)
