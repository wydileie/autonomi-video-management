import os
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

os.environ.setdefault("ADMIN_DB_USER", "test")
os.environ.setdefault("ADMIN_DB_PASS", "test")
os.environ.setdefault("ADMIN_DB_HOST", "localhost")
os.environ.setdefault("ADMIN_DB_NAME", "test")
os.environ.setdefault("ADMIN_USERNAME", "admin")
os.environ.setdefault("ADMIN_PASSWORD", "admin")
os.environ.setdefault("ADMIN_AUTH_SECRET", "test-secret")
os.environ.setdefault("CORS_ALLOWED_ORIGINS", "http://localhost")

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from src import admin_service as admin


class AdminConfigTests(unittest.TestCase):
    def validate(
        self,
        *,
        app_env: str | None = None,
        environment: str | None = None,
        username: str = "admin",
        password: str = "admin",
        secret: str = "test-secret",
        ttl_hours: int = 12,
    ) -> None:
        env_patch = {"APP_ENV": "", "ENVIRONMENT": ""}
        if app_env is not None:
            env_patch["APP_ENV"] = app_env
        if environment is not None:
            env_patch["ENVIRONMENT"] = environment

        with patch.dict(os.environ, env_patch, clear=False):
            admin._validate_admin_auth_config(username, password, secret, ttl_hours)

    def test_local_defaults_are_allowed_outside_production(self):
        with patch.dict(
            os.environ,
            {"APP_ENV": "development", "ENVIRONMENT": ""},
            clear=False,
        ):
            admin._validate_admin_auth_config("admin", "admin", "admin", 12)

    def test_production_rejects_default_admin_values(self):
        with self.assertRaisesRegex(RuntimeError, "ADMIN_USERNAME"):
            self.validate(
                app_env="production",
                username="admin",
                password="strong-admin-password",
                secret="a" * 32,
            )

    def test_production_rejects_change_me_values(self):
        with self.assertRaisesRegex(RuntimeError, "ADMIN_PASSWORD"):
            self.validate(
                environment="prod",
                username="service-admin",
                password="change-me-now",
                secret="a" * 32,
            )

    def test_production_rejects_change_this_values(self):
        with self.assertRaisesRegex(RuntimeError, "ADMIN_PASSWORD"):
            self.validate(
                environment="prod",
                username="service-admin",
                password="ChangeThisUploaderPassword",
                secret="a" * 32,
            )

    def test_production_rejects_secret_equal_to_password(self):
        shared = "same-long-secret-value-for-prod"
        with self.assertRaisesRegex(RuntimeError, "ADMIN_AUTH_SECRET must not equal"):
            self.validate(
                app_env="production",
                username="service-admin",
                password=shared,
                secret=shared,
            )

    def test_production_rejects_short_password(self):
        with self.assertRaisesRegex(RuntimeError, "ADMIN_PASSWORD must be at least"):
            self.validate(
                app_env="production",
                username="service-admin",
                password="short-pass",
                secret="a" * 32,
            )

    def test_production_rejects_short_secret(self):
        with self.assertRaisesRegex(RuntimeError, "ADMIN_AUTH_SECRET must be at least"):
            self.validate(
                app_env="production",
                username="service-admin",
                password="strong-admin-password",
                secret="short-secret",
            )

    def test_production_accepts_strong_admin_auth(self):
        self.validate(
            app_env="production",
            username="service-admin",
            password="strong-admin-password",
            secret="a" * 32,
        )

    def test_ttl_must_be_positive_in_all_environments(self):
        with self.assertRaisesRegex(RuntimeError, "ADMIN_AUTH_TTL_HOURS"):
            self.validate(app_env="development", ttl_hours=0)

    def test_ttl_parser_rejects_non_integer_values(self):
        with self.assertRaisesRegex(RuntimeError, "ADMIN_AUTH_TTL_HOURS must be an integer"):
            admin._parse_admin_auth_ttl_hours("twelve")


if __name__ == "__main__":
    unittest.main()
