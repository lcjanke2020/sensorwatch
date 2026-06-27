"""sensorwatch — entry point.

Reads HWiNFO64 sensor data via shared memory at a configurable interval
and writes JSON Lines logs with daily file rotation.

Usage:
    python -m sensorwatch [--config path/to/config.toml]
"""

from __future__ import annotations

import argparse
import logging
import signal
import sys
import time
from pathlib import Path

from .config import Config
from .hwinfo_shm import read_sensors
from .logger import SensorLogger

log = logging.getLogger("sensorwatch")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="sensorwatch",
        description="Monitor hardware sensors via HWiNFO64 shared memory",
    )
    parser.add_argument(
        "--config", "-c",
        type=Path,
        default=None,
        help="Path to config.toml (default: looks for config.toml next to package)",
    )
    parser.add_argument(
        "--verbose", "-v",
        action="store_true",
        help="Enable debug logging",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)-8s %(name)s — %(message)s",
        datefmt="%H:%M:%S",
    )

    # The reader depends on HWiNFO64's Windows shared memory; fail fast rather
    # than spinning and logging the same error every interval elsewhere.
    if sys.platform != "win32":
        log.error("sensorwatch requires Windows (HWiNFO64 shared memory); platform is %s.", sys.platform)
        raise SystemExit(1)

    config = Config.load(args.config)
    log.info(
        "Starting sensorwatch: interval=%ds, log_dir=%s, retention=%d days",
        config.interval_seconds, config.log_dir, config.retention_days,
    )
    if config.sensor_include:
        log.info("Sensor filter (include): %s", config.sensor_include)
    else:
        log.info("Sensor filter: capturing ALL sensors")

    # Graceful shutdown
    shutdown = False

    def on_signal(signum, frame):
        nonlocal shutdown
        log.info("Received signal %s, shutting down...", signal.Signals(signum).name)
        shutdown = True

    signal.signal(signal.SIGINT, on_signal)
    if hasattr(signal, "SIGTERM"):
        signal.signal(signal.SIGTERM, on_signal)
    if sys.platform == "win32" and hasattr(signal, "SIGBREAK"):
        signal.signal(signal.SIGBREAK, on_signal)

    hwinfo_warned = False
    with SensorLogger(config.log_dir, config.retention_days) as logger:
        while not shutdown:
            readings = read_sensors()

            if readings is None:
                if not hwinfo_warned:
                    log.warning("HWiNFO64 shared memory not available — is it running with shared memory enabled?")
                    hwinfo_warned = True
            else:
                filtered = [r for r in readings if config.matches_sensor(r.sensor_name)]
                if filtered:
                    logger.write([r.to_dict() for r in filtered])
                    log.debug("Logged %d readings", len(filtered))
                else:
                    log.debug("No readings matched sensor filters")

            # Sleep in small increments so we can respond to signals promptly.
            # Guard against a zero/non-int interval slipping through to range().
            for _ in range(max(1, int(config.interval_seconds * 10))):
                if shutdown:
                    break
                time.sleep(0.1)

    log.info("sensorwatch stopped.")


if __name__ == "__main__":
    main()
