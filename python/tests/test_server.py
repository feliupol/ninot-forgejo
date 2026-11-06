"""Exercise actual Git state and HTTP clients through the native fixture."""

import ssl
from pathlib import Path

import httpx

from ninot_forgejo import Server


def test_compare_status_and_squash_merge() -> None:
    with Server() as server:
        repository = server.create_user("pol").create_repository("app", private=True)
        repository.commit_file("main", ".github/github-automations.yml", "enabled: true\n")
        sha = repository.commit_file("feature", "module.py", "VALUE = 1\n")
        with httpx.Client(
            base_url=server.api_url,
            headers={"Authorization": f"token {server.admin_token}"},
        ) as client:
            root = "/repos/pol/app"
            comparison = client.get(f"{root}/compare/main...feature")
            assert comparison.status_code == 200, comparison.text
            assert comparison.json()["total_commits"] == 1
            assert comparison.json()["files"][0]["filename"] == "module.py"
            for value in ["failure", "success"]:
                response = client.post(
                    f"{root}/statuses/{sha}", json={"state": value, "context": "check"}
                )
                assert response.status_code == 201, response.text
            combined = client.get(f"{root}/commits/feature/status").json()
            assert combined["state"] == "success"
            assert combined["total_count"] == 1
            assert len(client.get(f"{root}/statuses/{sha}").json()) == 2
            response = client.post(
                f"{root}/pulls",
                json={"title": "Update", "base": "main", "head": "feature"},
            )
            assert response.status_code == 201, response.text
            pull_request = server.repository("pol", "app").pull_request(response.json()["number"])
            index = pull_request.index
            assert client.get(f"{root}/pulls", params={"head": "missing"}).json() == []
            response = client.post(f"{root}/pulls/{index}/merge", json={"Do": "squash"})
            assert response.status_code == 200, response.text
            assert client.get(f"{root}/pulls/{index}").json()["merged"] is True
            assert (
                client.get(f"{root}/contents/module.py", params={"ref": "main"}).json()["content"]
                == "VkFMVUUgPSAxCg=="
            )


def test_stopped_fixture_rejects_reuse() -> None:
    import pytest

    server = Server()
    server.stop()
    with pytest.raises(RuntimeError, match="stopped"):
        server.create_user("pol")


def test_file_mutations_checks_and_scheduled_merge_share_git_state() -> None:
    with Server() as server:
        repository = server.create_user("pol").create_repository("app")
        label = repository.create_label("automerge")
        with httpx.Client(
            base_url=server.api_url,
            headers={"Authorization": f"token {server.admin_token}"},
        ) as client:
            root = "/repos/pol/app"
            response = client.post(f"{root}/branches", json={"new_branch_name": "feature"})
            assert response.status_code == 201, response.text
            assert (
                client.post(f"{root}/branches", json={"new_branch_name": "feature"}).status_code
                == 409
            )
            response = client.post(
                f"{root}/contents/.github/config.yml",
                json={"content": "eDogMQo=", "branch": "feature", "message": "Add config"},
            )
            assert response.status_code == 201, response.text
            sha = response.json()["commit"]["sha"]
            assert client.get(f"{root}/branches/feature").json()["commit"]["id"] == sha
            pull_request = repository.create_pull_request(head="feature")
            index = pull_request.index
            response = client.post(f"{root}/issues/{index}/labels", json={"labels": [label]})
            assert response.status_code == 200, response.text
            assert client.get(f"{root}/pulls/{index}").json()["labels"][0]["name"] == "automerge"
            repository.create_action_task({"status": "running", "head_sha": sha, "name": "Build"})
            assert client.get(f"{root}/actions/tasks").json()["workflow_runs"][0]["name"] == "Build"
            response = client.post(
                f"{root}/statuses/{sha}", json={"state": "pending", "context": "check"}
            )
            assert response.status_code == 201
            schedule = {
                "Do": "squash",
                "merge_when_checks_succeed": True,
                "delete_branch_after_merge": True,
            }
            assert client.post(f"{root}/pulls/{index}/merge", json=schedule).status_code == 200
            scheduled = pull_request.scheduled_merge()
            assert scheduled is not None
            assert (scheduled.merge_method, scheduled.delete_branch) == ("squash", True)
            assert repository.advance() == 0
            assert client.post(f"{root}/pulls/{index}/merge", json=schedule).status_code == 409
            assert client.delete(f"{root}/pulls/{index}/merge").status_code == 204
            assert pull_request.scheduled_merge() is None
            assert client.post(f"{root}/pulls/{index}/merge", json=schedule).status_code == 200
            assert (
                client.post(
                    f"{root}/statuses/{sha}", json={"state": "success", "context": "check"}
                ).status_code
                == 201
            )
            assert repository.advance() == 1
            assert pull_request.scheduled_merge() is None
            assert client.get(f"{root}/pulls/{index}").json()["merged"] is True
            assert client.get(f"{root}/branches/feature").status_code == 404
            assert client.get(f"{root}/contents/.github/config.yml").json()["content"] == "eDogMQo="


def test_faults_are_single_use_and_profiles_observe_network_requests() -> None:
    with Server() as server:
        server.create_user("pol").create_repository("app")
        path = "/api/v1/repos/pol/app"
        server.fail_next("GET", path, 503)
        server.start_profile()
        with httpx.Client(headers={"Authorization": f"token {server.admin_token}"}) as client:
            assert client.get(server.url + path).status_code == 503
            assert client.get(server.url + path).status_code == 200
        hits = server.stop_profile()
        assert [(hit["method"], hit["path"], hit["status"]) for hit in hits] == [
            ("GET", path, 503),
            ("GET", path, 200),
        ]


def test_scheduled_merge_waits_for_the_checks_on_a_changed_head() -> None:
    with Server() as server:
        repository = server.create_user("pol").create_repository("app")
        old_sha = repository.commit_file("feature", "app.py", "VALUE = 1\n")
        pull_request = repository.create_pull_request(head="feature")
        index = pull_request.index
        with httpx.Client(
            base_url=server.api_url, headers={"Authorization": f"token {server.admin_token}"}
        ) as client:
            root = "/repos/pol/app"
            assert (
                client.post(
                    f"{root}/statuses/{old_sha}", json={"state": "pending", "context": "check"}
                ).status_code
                == 201
            )
            assert (
                client.post(
                    f"{root}/pulls/{index}/merge",
                    json={"Do": "squash", "merge_when_checks_succeed": True},
                ).status_code
                == 200
            )
            new_sha = repository.commit_file("feature", "app.py", "VALUE = 2\n")
            assert (
                client.post(
                    f"{root}/statuses/{old_sha}", json={"state": "success", "context": "check"}
                ).status_code
                == 201
            )
            assert repository.advance() == 0
            assert (
                client.post(
                    f"{root}/statuses/{new_sha}", json={"state": "success", "context": "check"}
                ).status_code
                == 201
            )
            assert repository.advance() == 1


def test_search_uses_shared_pr_author_and_reviewer_state() -> None:
    with Server() as server:
        repository = server.create_user("pol").create_repository("app")
        repository.commit_file("feature", "app.py", "VALUE = 1\n")
        pull_request = repository.create_pull_request(head="feature", author="alice")
        index = pull_request.index
        pull_request.request_review("bob")
        reviewer = server.user("bob")
        reviewer.set_profile("Bob Reviewer")
        token = reviewer.create_token(["read:repository", "read:issue", "read:user"])
        with httpx.Client(
            base_url=server.api_url, headers={"Authorization": f"token {token}"}
        ) as client:
            assert client.get("/user").json()["full_name"] == "Bob Reviewer"
            assert (
                client.get(
                    "/repos/issues/search", params={"type": "pulls", "created": "true"}
                ).json()
                == []
            )
            reviewing = client.get(
                "/repos/issues/search", params={"type": "pulls", "review_requested": "true"}
            )
            assert reviewing.status_code == 200, reviewing.text
            assert reviewing.json()[0]["number"] == index
            assert reviewing.json()[0]["repository"]["full_name"] == "pol/app"


def test_runs_filter_by_commit_and_serve_the_logs_of_executed_jobs() -> None:
    with Server() as server:
        repository = server.create_user("pol").create_repository("app")
        sha = repository.commit_file("main", "module.py", "VALUE = 1\n")
        other = repository.commit_file("main", "module.py", "VALUE = 2\n")
        run = repository.create_action_run(
            {"commit_sha": sha, "status": "failure"},
            [{"name": "lint", "status": "success"}, {"name": "test", "status": "failure"}],
        )
        repository.create_action_run({"commit_sha": other}, [{"name": "test"}])
        run.set_job_log(1, "FAILED tests/test_module.py::test_value\n")
        with httpx.Client(
            base_url=server.api_url,
            headers={"Authorization": f"token {server.admin_token}"},
        ) as client:
            root = "/repos/pol/app/actions"
            runs = client.get(f"{root}/runs", params={"head_sha": sha}).json()["workflow_runs"]
            assert [found["id"] for found in runs] == [run.id]
            lint, test = client.get(f"{root}/runs/{run.id}/jobs").json()
            log = client.get(f"{root}/jobs/{test['id']}/logs")
            assert log.status_code == 200, log.text
            assert log.text == "FAILED tests/test_module.py::test_value\n"
            unexecuted = client.get(f"{root}/jobs/{lint['id']}/logs")
            assert unexecuted.status_code == 404
            assert unexecuted.json()["message"] == "job has not been executed yet"


def test_tls_server_exposes_ca_bundle() -> None:
    with Server(tls=True) as server:
        assert server.url.startswith("https://")
        certificate = server.ca_cert_path
        assert certificate is not None
        bundle = Path(certificate)
        assert bundle.is_file()
        assert "BEGIN CERTIFICATE" in bundle.read_text(encoding="utf-8")
        response = httpx.get(
            f"{server.api_url}/version", verify=ssl.create_default_context(cafile=certificate)
        )
        assert response.status_code == 200, response.text
    assert Server().ca_cert_path is None
