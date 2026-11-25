"""A key pasted where its NAME belongs must never be stored, listed or repeated.

Names are readable by design: lapis.toml (0644), GET /v1/config, the secrets
list, error messages, and through those the chat history sent to the provider.
"""

from __future__ import annotations

import asyncio
import re
from pathlib import Path

import pytest
from aiohttp.test_utils import TestClient, TestServer
from conftest import make_core

from lapis_daemon import config as cfg
from lapis_daemon.api.server import build_app
from lapis_daemon.llm.anthropic_cua import AnthropicPlanner
from lapis_daemon.llm.base import LLMError
from lapis_daemon.llm.openai_cua import OpenAICompatPlanner

EXAMPLES = Path(__file__).resolve().parents[2] / "config"
TOKEN = "test-token-0123456789"
AUTH = {"Authorization": f"Bearer {TOKEN}"}

# Made-up values in the shapes real keys have; none of them is a key. The first
# six are valid secret NAMES by their characters, which is how they got through.
KEY_SHAPED = {
    "Groq": "gsk_" + "aB3dE5gH" * 6 + "wxyz",
    "Mistral": "Zx9Yw8Vu" * 4,
    "Gemini": "AIzaSy" + "D4eF5gH6" * 4 + "I",
    "Together": "a0b1c2d3" * 8,
    "Hugging Face": "hf_" + "Qr5St6Uv" * 4 + "Wx",
    "Fireworks": "fw_" + "3kT9mW2xLp7QaZ4vBn6RcY8d",
    "JWT": "eyJhbGciOiJIUzI1NiJ9." + "eyJzdWIiOiIxMjM0In0." + "c2lnbmF0dXJl",
    "Anthropic": "sk-ant-api03-" + "Ab1_cD2-" * 11 + "AA",
    "OpenRouter": "sk-or-v1-" + "0a1b2c3d" * 8,
    "OpenAI": "sk-proj-" + "Qr5St6Uv" * 7,
    "xAI": "xai-" + "Mn7Op8Qr" * 10,
    "Tailscale": "tskey-auth-" + "kQ3x9Zp2CNTRL",
}
VALID_AS_NAMES = ("Groq", "Mistral", "Gemini", "Together", "Hugging Face", "Fireworks")


def legitimate_names() -> set[str]:
    """Every default in the code and the examples, and names owners really use."""
    core, example = cfg.CoreConfig(), cfg.load_config(EXAMPLES / "lapis.example.toml")
    names = {c.llm.api_key_env for c in (core, example)}
    names |= {c.voice.stt_api_key_env for c in (core, example)}
    hosts = (cfg.mock_hosts(), cfg.load_hosts(EXAMPLES / "hosts.example.toml"))
    names |= {h.vnc.password_env for config in hosts for h in config.hosts.values()}
    names |= set(cfg.read_env_file(EXAMPLES / "secrets.example.env"))
    assert {"ANTHROPIC_API_KEY", "OPENAI_API_KEY", "LAPIS_VNC_PASSWORD_arch_dev"} <= names
    return names | {
        "MINIMAX_API_KEY", "AZURE_OPENAI_API_KEY_EASTUS2", "GOOGLE_GENERATIVE_AI_API_KEY",
        "MiniMaxAPI", "my_key", "LAPIS_VNC_PASSWORD_office_mac_mini", "INTERNATIONALIZATION_KEY",
        "OPENROUTER_API_KEY", "LAPIS_TS_AUTHKEY", "_K",
    }  # fmt: skip


def test_a_key_is_told_apart_from_its_name() -> None:
    looks = cfg.looks_like_secret_value
    for shape, value in KEY_SHAPED.items():
        assert looks(value), shape
        assert looks(f"  {value}\n") and looks(value.upper()) and looks(value.lower()), shape
    for name in legitimate_names() | {"minimax api key", "Groq Key 2"}:  # also as typed
        assert not looks(name), name
    for shape in VALID_AS_NAMES:  # the heuristic is all that stands between these and a name
        assert cfg.SECRET_NAME_RE.fullmatch(KEY_SHAPED[shape]), shape
    # The rule at its edges. A key prefix, in any case; a name may begin almost like one.
    for prefix in ("sk-", "gsk_", "AIza", "eyJ", "tskey-", "tskey_auth"):
        assert looks(prefix + "x") and looks(prefix.swapcase() + "x"), prefix
    for name in ("SK_OPENROUTER", "sk_minimax", "TSKEY_HOME", "tskey", "gsk", "ask-x", "skx", ""):
        assert not looks(name), name
    # One run between separators: 12 characters and three switches between digits and
    # the rest, both needed.
    assert looks("abcdefghi1j2") and not looks("abcdefgh1j2")  # 12 characters, then 11
    assert not looks("abcdefghij12") and not looks("abcdefghi1jk")  # one switch, then two
    for separator in "_-. ":  # each one ends a run; nothing else does
        assert not looks(f"abcde1f2{separator}abcde1f2"), separator
    assert looks("abcde1f2abcde1f2") and looks("abcde1f2/abcde1f2")
    # The whole text without its separators (a key written in groups): 20 letters and
    # digits and six switches, both needed.
    grouped = "ab1_cd2_ef3_gh4_ij5_kl6_mn"
    assert looks(grouped) and not looks(grouped[:-1])  # 20 letters and digits, then 19
    assert not looks("abcd_1_efgh_2_ijklmnopqrs_3")  # 22 of them, five switches
    assert looks("abcd_1_efgh_2_ijklmnopqrs_3_x")  # ...and the sixth
    # Length alone says nothing: words stay a name, however many.
    assert not looks("A" * 64) and not looks("my_" * 21 + "key") and not looks("9" * 64)


# -- the same rule as the Android app ---------------------------------------------------
# The app screens what is typed with ConfigRules.looksLikeSecretValue and proves it on
# two lists in its own test: `keyShaped` (must be refused) and `names` (must stay
# accepted). The device is the authority, so it is held to the very same lists, read
# from the Kotlin sources: a case added there is a case here.

ANDROID = Path(__file__).resolve().parents[2] / "android" / "app" / "src"
APP_TEST = ANDROID / "test" / "java" / "dev" / "lapiszero" / "app" / "ui" / "ViewModelTest.kt"
APP_RULES = (
    ANDROID / "main" / "java" / "dev" / "lapiszero" / "app" / "ui" / "config" / "ConfigViewModel.kt"
)
# One Kotlin string without escapes or templates, perhaps repeated: "Ab1_".repeat(11)
_KOTLIN_TERM = re.compile(r'"([^"\\$]*)"(?:\.repeat\((\d+)\))?')


def kotlin_strings(source: str, opening: str) -> list[str]:
    """The values of the `listOf(...)` that `opening` (ending in its `(`) begins."""
    items: list[str] = []
    item, depth, i = "", 1, source.index(opening) + len(opening)
    while depth:
        c = source[i]
        if c == '"':  # a literal, whole: commas and parentheses in it are text
            end = source.index('"', i + 1) + 1
            item, i = item + source[i:end], end
            continue
        if source.startswith("//", i):
            i = source.index("\n", i)
            continue
        depth += (c == "(") - (c == ")")
        if depth == 0 or (depth == 1 and c == ","):
            items.append(item.strip())
            item = ""
        else:
            item += c
        i += 1
    values = []
    for expr in filter(None, items):
        # Nothing but literals, `.repeat(n)` and `+`: anything else needs a look here.
        assert not _KOTLIN_TERM.sub("", expr).replace("+", "").strip(), expr
        values.append("".join(text * int(n or 1) for text, n in _KOTLIN_TERM.findall(expr)))
    return values


def tidied(text: str) -> str:
    """ConfigRules.normalizeSecretName: what the app makes of a typed name before sending it."""
    text = re.sub(r"[\s.-]+", "_", text)
    return "".join(c for c in text if c.isascii() and (c.isalnum() or c == "_")).lstrip(
        "0123456789"
    )[:64]


def test_reading_the_apps_lists() -> None:
    kotlin = """
        private val keyShaped = listOf(
            "sk-ant-" + "Ab1_".repeat(3) + "AA", // a comment, with "quotes" and a (
            "plain, with a comma (and a parenthesis)",
            // a line of its own
            "x".repeat(2) + "y" + "z".repeat(3),
        )
        val other = listOf("not this one")
    """
    assert kotlin_strings(kotlin, "val keyShaped = listOf(") == [
        "sk-ant-Ab1_Ab1_Ab1_AA",
        "plain, with a comma (and a parenthesis)",
        "xxyzzz",
    ]
    for unread in ('listOf("a" + suffix)', 'listOf("tab\\t")', 'listOf("$name")'):
        with pytest.raises(AssertionError):
            kotlin_strings(unread, "listOf(")
    assert tidied(" open-router.key 2 ") == "_open_router_key_2_" and tidied("9 lives") == "_lives"


def test_the_device_and_the_app_agree_on_every_key_and_every_name(tmp_path: Path) -> None:
    # Moved or renamed? Point the three paths above at the new place: do not skip this.
    assert APP_TEST.is_file() and APP_RULES.is_file(), "the Android sources are not there"
    test_source, rules_source = APP_TEST.read_text(), APP_RULES.read_text()
    keys = kotlin_strings(test_source, "private val keyShaped = listOf(")
    # `names` is the presets' key names plus a list of its own.
    opening = "val names = ProviderPreset.ALL.map { it.keyEnv } + listOf("
    presets = re.findall(r'ProviderPreset\((?:"[^"]*",\s*){3}"([^"]*)"', rules_source)
    names = presets + kotlin_strings(test_source, opening)
    # The parser found the lists, not something else: their size, and entries that
    # prove the reading (a `.repeat()` worked out, a preset, a name with spaces).
    assert len(keys) >= 10 and len(presets) >= 5 and len(names) >= 15
    assert "sk-ant-api03-" + "Ab1_cD2-" * 11 + "AA" in keys
    assert "ANTHROPIC_API_KEY" in presets and "Groq Key 2" in names
    # The app's prefixes are the device's (compared without regard to case on both sides).
    listed = re.search(r"KEY_PREFIXES = listOf\(([^)]*)\)", rules_source)
    assert listed and tuple(p.lower() for p in re.findall(r'"([^"]*)"', listed[1])) == (
        cfg._KEY_PREFIXES
    )

    looks, path = cfg.looks_like_secret_value, tmp_path / "secrets.env"
    for key in keys:
        # As pasted, and as the app would tidy it into a name (that was the hole).
        for form in (key, tidied(key), tidied(key).upper()):
            assert looks(form), form
            assert not cfg.is_key_name(form), form
            with pytest.raises(ValueError):
                cfg.HostSsh(password_env=form)
            with pytest.raises(ValueError) as err:
                cfg.write_env_file(path, {form: "value"})
            assert form[:12] not in str(err.value), form  # the refusal does not repeat it
    assert not path.exists()
    for name in names:
        assert not looks(name), name
        name = tidied(name)  # what the app sends
        assert not looks(name) and cfg.is_key_name(name), name
        assert cfg.LLMConfig(api_key_env=name).api_key_env == name
        assert cfg.HostSsh(password_env=name).password_env == name
        cfg.write_env_file(path, {name: "value"})  # as a new name, each time
        assert cfg.read_env_file(path) == {name: "value"}


def test_llm_config_refuses_a_key_as_the_key_name_and_keeps_real_names() -> None:
    for shape, value in KEY_SHAPED.items():
        with pytest.raises(ValueError) as err:
            cfg.CoreConfig.model_validate({"llm": {"api_key_env": value}})
        # PUT /v1/config answers with this text: it names the field, never the input.
        assert "llm.api_key_env" in str(err.value), shape
        assert value[:10] not in str(err.value) and value[-10:] not in str(err.value), shape
        assert not cfg.is_key_name(value), shape
    for name in legitimate_names():
        assert cfg.LLMConfig(api_key_env=name).api_key_env == name
        assert cfg.is_key_name(name)


def test_a_new_secret_name_that_reads_like_a_key_is_refused(tmp_path: Path) -> None:
    path = tmp_path / "secrets.env"
    cfg.write_env_file(path, {"MINIMAX_API_KEY": "value-1"})
    for shape, value in KEY_SHAPED.items():  # name and value swapped
        with pytest.raises(ValueError, match="secret name looks like a key") as err:
            cfg.write_env_file(path, {"MINIMAX_API_KEY": "value-1", value: "MINIMAX_API_KEY"})
        assert value[:10] not in str(err.value) and value[-10:] not in str(err.value), shape
    assert cfg.read_env_file(path) == {"MINIMAX_API_KEY": "value-1"}  # nothing of it is stored

    # A name the file already holds stays writable: the whole file is rewritten on every
    # change, so refusing it would block every other secret. It can be unset as before.
    old = KEY_SHAPED["Mistral"]
    path.write_text(f"MINIMAX_API_KEY=value-1\n{old}=stored-by-an-older-daemon\n")
    cfg.write_env_file(path, cfg.read_env_file(path) | {"OPENAI_API_KEY": "value-2", old: "new"})
    assert cfg.read_env_file(path) == {
        "MINIMAX_API_KEY": "value-1",
        "OPENAI_API_KEY": "value-2",
        old: "new",
    }
    cfg.write_env_file(path, {"MINIMAX_API_KEY": "value-1"})
    with pytest.raises(ValueError, match="looks like a key"):  # gone, so new again
        cfg.write_env_file(path, {old: "value"})

    # The names the app derives from a host id are long and mixed by nature: exempt
    # from the heuristic, not from the character rule or the reserved names.
    derived = "LAPIS_VNC_PASSWORD_" + "a1b2c3d4e5" * 4
    assert cfg.looks_like_secret_value(derived) and len(derived) == 59
    cfg.write_env_file(path, {derived: "screen-sharing-password"})
    assert cfg.read_env_file(path) == {derived: "screen-sharing-password"}
    with pytest.raises(ValueError, match="invalid secret name"):
        cfg.write_env_file(path, {"LAPIS_VNC_PASSWORD_office-mac-mini": "x"})
    # A password does not read like a key. Swapped with its name it is no name either,
    # and that refusal does not repeat it.
    with pytest.raises(ValueError, match="invalid secret name") as err:
        cfg.write_env_file(path, {"correct horse battery staple": "LAPIS_VNC_PASSWORD_mac"})
    assert "horse" not in str(err.value) and not cfg.looks_like_secret_value("correct horse")
    with pytest.raises(ValueError, match="reserved"):
        cfg.write_env_file(path, {"LAPIS_CONFIG_DIR": "/tmp/elsewhere"})
    names = dict.fromkeys(legitimate_names(), "value")
    cfg.write_env_file(path, names)
    assert cfg.read_env_file(path) == names


@pytest.mark.parametrize("planner_type", [AnthropicPlanner, OpenAICompatPlanner])
@pytest.mark.parametrize("name", [KEY_SHAPED["Groq"], KEY_SHAPED["OpenAI"], "PATH", "", "a b"])
def test_a_planner_never_repeats_or_looks_up_a_key_name_that_is_not_a_name(
    planner_type: type, name: str
) -> None:
    # Validation refuses such a config; one built around it must still fail closed. The
    # text of this error becomes a chat message, an LCD toast and, as history, a prompt.
    # (PATH is set in any process, and is certainly not an API key.)
    config = cfg.LLMConfig(provider="openai").model_copy(update={"api_key_env": name})
    planner = planner_type(config)
    with pytest.raises(
        LLMError, match=r"^The API key name in Config > Agent & LLM is not a name"
    ) as err:
        asyncio.run(planner.start("task", host="mac", host_os="macos", history=[]))
    assert not name or name not in str(err.value)
    assert planner._session is None  # nothing was sent anywhere under that "key"


def test_a_key_as_a_name_is_refused_over_rest_and_shown_nowhere(short_tmp: Path) -> None:
    async def scenario() -> None:
        core = await make_core(short_tmp)
        await core.auth.register_fixed("dev_test", "Pixel", TOKEN)
        client = TestClient(TestServer(build_app(core)))
        await client.start_server()
        try:
            base = (await (await client.get("/v1/config", headers=AUTH)).json())["config"]
            swapped = {**KEY_SHAPED, "a passphrase": "correct horse battery staple 42"}
            for shape, value in swapped.items():
                llm = {**base["llm"], "provider": "openai", "api_key_env": value}
                put = await client.put(
                    "/v1/config", json={"config": {**base, "llm": llm}}, headers=AUTH
                )
                patch = await client.patch(
                    "/v1/secrets", json={"set": {value: "MINIMAX_API_KEY"}}, headers=AUTH
                )
                assert (put.status, patch.status) == (422, 422), shape
                shown = [await put.text(), await patch.text()]
                for path in ("/v1/config", "/v1/secrets/status", "/v1/state"):
                    shown.append(await (await client.get(path, headers=AUTH)).text())
                shown += [
                    p.read_text() for p in (core.paths.config, core.paths.secrets) if p.exists()
                ]
                assert not any(value in text or value[:10] in text for text in shown), shape
            # A name without a value is refused without being repeated either.
            name = KEY_SHAPED["Mistral"]
            empty = await client.patch("/v1/secrets", json={"set": {name: ""}}, headers=AUTH)
            assert empty.status == 422 and name[:10] not in await empty.text()
            assert core.config.llm.api_key_env == "ANTHROPIC_API_KEY"
        finally:
            await client.close()
            await core.stop()

    asyncio.run(scenario())
