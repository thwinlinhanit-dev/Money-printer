"""S3/B2 archive script (SPEC-011).

Uploads compacted Parquet files older than current UTC day to S3-compatible
object storage, verifies checksums, and purges local files only on success.

Usage:
  python research/archive_data.py

Environment:
  AWS_ACCESS_KEY_ID       S3-compatible access key
  AWS_SECRET_ACCESS_KEY   S3-compatible secret key
  AWS_ENDPOINT_URL        S3-compatible endpoint URL (e.g., https://s3.us-west-004.backblazeb2.com)
  AWS_BUCKET_NAME         Target bucket name
  DATA_DIR                Root data directory (default: ./data)
"""

import hashlib
import json
import logging
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%dT%H:%M:%SZ",
    handlers=[logging.StreamHandler(sys.stderr)],
)
logger = logging.getLogger("archive")


def now_utc() -> datetime:
    return datetime.now(timezone.utc)


def today_utc_str() -> str:
    return now_utc().strftime("%Y%m%d")


def md5_file(path: Path) -> str:
    h = hashlib.md5()
    with open(path, "rb") as f:
        while True:
            chunk = f.read(8 * 1024 * 1024)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()


def find_old_parquet_files(root: Path, today: str) -> list[Path]:
    """Find all .parquet files in `root` whose date partition is before today."""
    old: list[Path] = []
    for p in root.rglob("*.parquet"):
        # Skip files in today's partition
        rel = p.relative_to(root).as_posix()
        if f"date={today}" in rel:
            continue
        old.append(p)
    return sorted(old)


def find_old_raw_logs(root: Path, today: str) -> list[Path]:
    """Find raw .log files older than today (date prefix < today)."""
    old: list[Path] = []
    if not root.exists():
        return old
    for p in root.iterdir():
        if p.suffix != ".log":
            continue
        # filename format: YYYYMMDD_venue_symbol.log
        parts = p.stem.split("_", 1)
        if not parts:
            continue
        date_part = parts[0]
        if date_part < today and len(date_part) == 8:
            old.append(p)
    return sorted(old)


def upload_with_retry(
    s3_client,
    local_path: Path,
    bucket: str,
    key: str,
    max_retries: int = 3,
) -> bool:
    """Upload a file to S3 and verify integrity. Returns True on success."""
    md5_local = md5_file(local_path)
    file_size = local_path.stat().st_size

    for attempt in range(1, max_retries + 1):
        try:
            logger.info(
                "uploading %s -> s3://%s/%s (attempt %d/%d, size=%d, md5=%s)",
                local_path.name, bucket, key, attempt, max_retries, file_size, md5_local,
            )

            s3_client.upload_file(
                Filename=str(local_path),
                Bucket=bucket,
                Key=key,
                ExtraArgs={"ContentType": "application/octet-stream"},
            )

            # --- Verify integrity ---
            head = s3_client.head_object(Bucket=bucket, Key=key)
            remote_size = head.get("ContentLength", 0)
            remote_etag = head.get("ETag", "").strip('"')

            if remote_size != file_size:
                logger.error(
                    "size mismatch: local=%d remote=%d", file_size, remote_size,
                )
                continue

            # Standard S3 ETag is the MD5 for single-part uploads
            if remote_etag and remote_etag != md5_local:
                logger.error(
                    "md5 mismatch: local=%s remote=%s", md5_local, remote_etag,
                )
                continue

            logger.info(
                "verified OK: %s -> s3://%s/%s", local_path.name, bucket, key,
            )
            return True

        except Exception as e:
            logger.warning(
                "upload attempt %d/%d failed: %s", attempt, max_retries, e,
            )
            if attempt < max_retries:
                delay = 2 ** attempt
                logger.info("retrying in %ds...", delay)
                time.sleep(delay)

    return False


def main() -> None:
    today = today_utc_str()
    data_dir = Path(os.environ.get("DATA_DIR", "./data")).resolve()

    endpoint_url = os.environ.get("AWS_ENDPOINT_URL", "")
    bucket = os.environ.get("AWS_BUCKET_NAME", "")
    access_key = os.environ.get("AWS_ACCESS_KEY_ID", "")
    secret_key = os.environ.get("AWS_SECRET_ACCESS_KEY", "")

    if not (endpoint_url and bucket and access_key and secret_key):
        logger.error(
            "Missing S3 env vars. Set AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, "
            "AWS_ENDPOINT_URL, AWS_BUCKET_NAME"
        )
        sys.exit(1)

    try:
        import boto3
    except ImportError:
        logger.error("boto3 not installed. Run: pip install boto3")
        sys.exit(1)

    session = boto3.Session(
        aws_access_key_id=access_key,
        aws_secret_access_key=secret_key,
    )
    s3 = session.client("s3", endpoint_url=endpoint_url)

    # --- Find old Parquet files ---
    cold_dir = data_dir / "cold"
    if not cold_dir.exists():
        logger.info("No cold directory at %s; nothing to archive", cold_dir)
        sys.exit(0)

    parquet_files = find_old_parquet_files(cold_dir, today)
    if not parquet_files:
        logger.info("No old parquet files to archive (today=%s)", today)
    else:
        logger.info("Found %d old parquet files to archive", len(parquet_files))

    any_failure = False

    for local_path in parquet_files:
        # Compute S3 key relative to cold dir
        rel = local_path.relative_to(cold_dir).as_posix()
        key = f"cold/{rel}"

        ok = upload_with_retry(s3, local_path, bucket, key)
        if ok:
            logger.info("purging local file: %s", local_path)
            local_path.unlink()
        else:
            any_failure = True
            logger.error(
                "FAILED to archive %s after retries — NOT deleting local file",
                local_path,
            )

    # --- Purge old raw logs ---
    raw_dir = data_dir / "raw"
    raw_logs = find_old_raw_logs(raw_dir, today)
    for log_path in raw_logs:
        logger.info("purging old raw log: %s", log_path)
        log_path.unlink()

    if any_failure:
        logger.error("Archive completed with failures — some files retained locally")
        sys.exit(2)

    logger.info("Archive completed successfully")


if __name__ == "__main__":
    main()
