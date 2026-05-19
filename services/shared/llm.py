"""LLM (chat generation) provider abstraction.

Providers:
  openai     — OpenAI SDK (default). Also covers OpenAI-compatible endpoints
               (Gemini OpenAI mode, OpenRouter, Ollama, vLLM) via OPENAI_BASE_URL.
  anthropic  — Native Anthropic Messages API.

Environment variables:
  LLM_PROVIDER       — openai | anthropic (default: openai)
  LLM_MODEL          — chat model name (provider-specific)
  OPENAI_API_KEY     — used when LLM_PROVIDER=openai
  OPENAI_BASE_URL    — override (point at any OpenAI-compat endpoint)
  ANTHROPIC_API_KEY  — used when LLM_PROVIDER=anthropic
  ANTHROPIC_BASE_URL — optional override
"""
import os
from typing import Any

from openai import AsyncOpenAI
from openai.types.chat import ChatCompletionMessageParam


def _openai_client() -> AsyncOpenAI:
    return AsyncOpenAI(
        api_key=os.environ["OPENAI_API_KEY"],
        base_url=os.environ.get("OPENAI_BASE_URL") or None,
    )


async def generate(messages: list[ChatCompletionMessageParam]) -> str:
    """Generate a response from the LLM. Returns the assistant message content."""
    answer, _ = await generate_with_usage(messages)
    return answer


async def generate_with_usage(
    messages: list[ChatCompletionMessageParam],
) -> tuple[str, dict]:
    """Dispatch to the configured provider. Returns (answer, usage_dict)."""
    provider = os.environ.get("LLM_PROVIDER", "openai").lower()
    if provider == "anthropic":
        return await _generate_anthropic(messages)
    if provider == "openai":
        return await _generate_openai(messages)
    raise ValueError(f"Unknown LLM_PROVIDER: {provider!r}. Valid: openai, anthropic")


async def _generate_openai(
    messages: list[ChatCompletionMessageParam],
) -> tuple[str, dict]:
    model = os.environ.get("LLM_MODEL", "gpt-5.4-mini")
    resp = await _openai_client().chat.completions.create(model=model, messages=messages)
    usage = {
        "model": resp.model,
        "prompt_tokens": resp.usage.prompt_tokens if resp.usage else 0,
        "completion_tokens": resp.usage.completion_tokens if resp.usage else 0,
        "total_tokens": resp.usage.total_tokens if resp.usage else 0,
    }
    return resp.choices[0].message.content or "", usage


async def _generate_anthropic(messages: list[Any]) -> tuple[str, dict]:
    """Anthropic Messages API. system messages are split out as a top-level param."""
    from anthropic import AsyncAnthropic

    client = AsyncAnthropic(
        api_key=os.environ["ANTHROPIC_API_KEY"],
        base_url=os.environ.get("ANTHROPIC_BASE_URL") or None,
    )
    system = next((m["content"] for m in messages if m.get("role") == "system"), None)
    user_messages = [m for m in messages if m.get("role") != "system"]

    model = os.environ.get("LLM_MODEL", "claude-sonnet-4-6")
    max_tokens = int(os.environ.get("LLM_MAX_TOKENS", "4096"))

    kwargs: dict[str, Any] = {
        "model": model,
        "messages": user_messages,
        "max_tokens": max_tokens,
    }
    if system is not None:
        kwargs["system"] = system

    resp = await client.messages.create(**kwargs)
    answer = "".join(block.text for block in resp.content if block.type == "text")
    usage = {
        "model": resp.model,
        "prompt_tokens": resp.usage.input_tokens,
        "completion_tokens": resp.usage.output_tokens,
        "total_tokens": resp.usage.input_tokens + resp.usage.output_tokens,
    }
    return answer, usage
