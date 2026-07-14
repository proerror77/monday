import unittest

from scripts.psql_connection import psql_environment


class PsqlConnectionTests(unittest.TestCase):
    def test_maps_url_components_to_libpq_environment(self) -> None:
        environment = psql_environment(
            "postgresql://user:p%40ss@db.example:5433/ploy%2Dresearch"
            "?sslmode=require&connect_timeout=7&application_name=ploy-audit",
            {"PATH": "/usr/bin", "PGHOST": "stale"},
        )

        self.assertEqual(environment["PATH"], "/usr/bin")
        self.assertEqual(environment["PGHOST"], "db.example")
        self.assertEqual(environment["PGPORT"], "5433")
        self.assertEqual(environment["PGDATABASE"], "ploy-research")
        self.assertEqual(environment["PGUSER"], "user")
        self.assertEqual(environment["PGPASSWORD"], "p@ss")
        self.assertEqual(environment["PGSSLMODE"], "require")
        self.assertEqual(environment["PGCONNECT_TIMEOUT"], "7")
        self.assertEqual(environment["PGAPPNAME"], "ploy-audit")

    def test_rejects_unsupported_or_incomplete_urls(self) -> None:
        for value in [
            "mysql://user:pass@db/ploy",
            "postgresql://user:pass@db",
            "postgresql://user:pass@db/ploy?options=-c%20role%3Dadmin",
            "postgresql://user:pass@db/ploy#fragment",
        ]:
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    psql_environment(value, {})


if __name__ == "__main__":
    unittest.main()
