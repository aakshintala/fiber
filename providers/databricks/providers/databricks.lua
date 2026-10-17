-- Databricks AI Gateway model discovery (docs/model-routing.md, "Model
-- discovery"): one entry per Unity Catalog model service whose
-- `supported_api_types` hold the Responses API or the chat API, routed by
-- data (docs/model-routing.md, "openai-completions facts"): a service the
-- catalog's `chat` rows match as Claude, by its row's `provider_type` or its
-- `claude-` name, is `anthropic-messages` at the Claude URL with
-- `compat.bearer`; one holding the Responses API is `openai-responses`;
-- the rest are `openai-completions`, the only protocol that writes
-- `reasoning_effort`. A service ending `-image`, or matching an
-- `image_generation` row, is left out, as is one with no window: a model
-- without one is left out with `model_invalid` (docs/model-routing.md,
-- "What a provider extension declares"). Gateway metadata comes only from
-- the external-provider catalog, matched by `modelKey` and `metadataFor`;
-- the context-window pair 922000/128000 reads as 1,050,000. Discovery
-- pages each endpoint's `next_page_token` back as `page_token`, at most 20
-- pages each. Every request carries the package's header
-- (docs/configuration.md, "A provider's data"), and every failure names
-- its endpoint, never the key or the body.

-- The URL scheme, defined once: a test copy rewrites it with one
-- substitution, as the data file's `https://{workspace}` is rewritten.
local SCHEME = "https://"
local HEADER_NAME = "Databricks-Ai-Gateway-Request-Tags"
local HEADER_VALUE = '{"source":"fiber"}'
local MAX_PAGES = 20

local SERVICES_PATH = "/api/2.1/unity-catalog/model-services"
local SERVICES_QUERY = "?parent=schemas%2Fsystem.ai&page_size=100"
local SERVICES_FIELD = "model_services"
local CATALOG_PATH = "/api/ai-gateway/v2/external-provider-models"
local CATALOG_QUERY = "?page_size=1000"
local CATALOG_FIELD = "models"

-- Every byte outside A-Z, a-z, 0-9 and -_.~ becomes %XX.
local function encode(text)
  return (text:gsub("[^A-Za-z0-9%-_%.~]", function(c)
    return string.format("%%%02X", string.byte(c))
  end))
end

-- Whether the decoded JSON value is a list: a table with only numeric keys
-- and no holes. JSON null decodes to a light userdata, never nil, so an
-- absent items field is not a list either.
local function is_list(value)
  if type(value) ~= "table" then
    return false
  end
  local count = 0
  for key in pairs(value) do
    if type(key) ~= "number" then
      return false
    end
    count = count + 1
  end
  return count == #value
end

-- Every page of the endpoint at `path`: each `GET` carries the key as
-- `authorization: Bearer` with the package's header, and each
-- `next_page_token` comes back as `page_token`. At most `MAX_PAGES` pages
-- are requested: a 20th page that still names a token raises, with no 21st
-- request. Any other failure names the endpoint and the status, never the
-- key or the body.
local function pages(workspace, key, path, query, field)
  local items = {}
  local token = nil
  for _ = 1, MAX_PAGES do
    local url = SCHEME .. workspace .. path .. query
    if token ~= nil then
      url = url .. "&page_token=" .. encode(token)
    end
    local reply = host.http({
      method = "GET",
      url = url,
      headers = {
        authorization = "Bearer " .. key,
        [HEADER_NAME] = HEADER_VALUE,
      },
    })
    if reply.status ~= 200 then
      error("databricks models: " .. path .. ": unexpected status " .. tostring(reply.status), 0)
    end
    local ok, body = pcall(json.decode, reply.body)
    if not ok then
      error(
        "databricks models: " .. path .. ": status "
          .. tostring(reply.status)
          .. " with a body that is not JSON",
        0
      )
    end
    local page = type(body) == "table" and body[field] or nil
    if not is_list(page) then
      error(
        "databricks models: " .. path .. ": status "
          .. tostring(reply.status)
          .. " with no `"
          .. field
          .. "` list",
        0
      )
    end
    for _, item in ipairs(page) do
      items[#items + 1] = item
    end
    local next_token = body.next_page_token
    if type(next_token) ~= "string" then
      return items
    end
    token = next_token
  end
  error("databricks models: " .. path .. ": the listing has more than 20 pages", 0)
end

-- The value without one leading prefix: each group below strips at most
-- one, in the same order.
local function strip_one(value, prefixes)
  for _, prefix in ipairs(prefixes) do
    if value:sub(1, #prefix) == prefix then
      return value:sub(#prefix + 1)
    end
  end
  return value
end

-- The match key of a service or catalog name: lower-cased, each vendor
-- prefix stripped once, the part after the last `/`, runs outside `a-z0-9`
-- as one `-`, one trailing `-maas`, `-preview` or `-it` dropped, and one
-- leading and trailing `-` trimmed.
local function model_key(raw)
  local value = raw:lower()
  value = value:gsub("^model%-services/system%.ai%.", "")
  value = value:gsub("^system%.ai%.", "")
  value = value:gsub("^databricks%-", "")
  value = value:gsub("^fw%-", "")
  value = value:gsub("^meta%-", "")
  value = strip_one(value, { "global.", "us.", "eu.", "apac." })
  value = strip_one(value, { "xai.", "anthropic.", "openai.", "google." })
  value = value:match("[^/]*$")
  value = value:gsub("[^a-z0-9]+", "-")
  local stem = value:match("%-(%a+)$")
  if stem == "maas" or stem == "preview" or stem == "it" then
    value = value:sub(1, -(#stem + 2))
  end
  value = value:gsub("^%-", ""):gsub("%-$", "")
  return value
end

-- The catalog's likely backend for a bare service name.
local function preferred_provider(bare)
  if bare:sub(1, 4) == "gpt-" then
    return "OPENAI"
  end
  if bare:sub(1, 7) == "gemini-" then
    return "GEMINI_ENTERPRISE"
  end
  if bare:sub(1, 7) == "claude-" then
    return "ANTHROPIC"
  end
  return "MICROSOFT_FOUNDRY"
end

-- The catalog's best `chat` row for a bare service name: an exact key wins
-- at 1000; a dated alias (the key is the candidate, `-`, and 4 to 8
-- digits) or an expanded alias (the candidate starts with the key and
-- `-`) scores `500 - |length difference|`. Ties break toward the likely
-- backend, then toward the earlier row, in one scan.
local function metadata_for(bare, catalog)
  local key = model_key(bare)
  local prefer = preferred_provider(bare)
  local best = nil
  local best_score = -1
  local best_preferred = false
  for _, row in ipairs(catalog) do
    if type(row) == "table" and row.mode == "chat" and type(row.model_name) == "string" then
      local candidate = model_key(row.model_name)
      local score = -1
      if candidate == key then
        score = 1000
      else
        local dated = key:sub(1, #candidate + 1) == candidate .. "-"
          and key:sub(#candidate + 2):match("^%d%d%d%d%d?%d?%d?%d?$") ~= nil
        local expanded = candidate:sub(1, #key + 1) == key .. "-"
        if dated or expanded then
          score = 500 - math.abs(#candidate - #key)
        end
      end
      if score >= 0 then
        local preferred = row.provider_type == prefer
        if score > best_score or (score == best_score and preferred and not best_preferred) then
          best = row
          best_score = score
          best_preferred = preferred
        end
      end
    end
  end
  return best
end

-- The JSON number, or nil when absent or not a number.
local function number(value)
  if type(value) == "number" then
    return value
  end
  return nil
end

-- One price in US dollars per million tokens: each price reads from its
-- `*_cost_per_million_tokens` spelling, else its `*_per_million_tokens`
-- spelling, else the fallback. An explicit 0 stays 0: only nil falls
-- through, as 0 is truthy.
local function price(raw, cost_name, plain_name, fallback)
  return number(raw[cost_name]) or number(raw[plain_name]) or fallback
end

-- The entry's `cost` from a catalog row's `base_pricing`: tiers from
-- `long_context`, each with `threshold_tokens` as `input_tokens_above`
-- and each missing tier price taking the base price, a missing base cache
-- price reading as 0. No cost when input and output are both 0 or absent.
local function cost_from(pricing)
  if type(pricing) ~= "table" then
    return nil
  end
  local base_input = price(pricing, "input_cost_per_million_tokens", "input_per_million_tokens", 0)
  local base_output =
    price(pricing, "output_cost_per_million_tokens", "output_per_million_tokens", 0)
  local base_read =
    price(pricing, "cache_read_input_cost_per_million_tokens", "cache_read_per_million_tokens", 0)
  local base_write = price(
    pricing,
    "cache_creation_input_cost_per_million_tokens",
    "cache_write_per_million_tokens",
    0
  )
  local tiers = {}
  if is_list(pricing.long_context) then
    for _, raw in ipairs(pricing.long_context) do
      if type(raw) == "table" then
        local threshold = number(raw.threshold_tokens)
        if threshold ~= nil then
          tiers[#tiers + 1] = {
            input_tokens_above = threshold,
            input = price(raw, "input_cost_per_million_tokens", "input_per_million_tokens", base_input),
            output = price(
              raw,
              "output_cost_per_million_tokens",
              "output_per_million_tokens",
              base_output
            ),
            cache_read = price(
              raw,
              "cache_read_input_cost_per_million_tokens",
              "cache_read_per_million_tokens",
              base_read
            ),
            cache_write = price(
              raw,
              "cache_creation_input_cost_per_million_tokens",
              "cache_write_per_million_tokens",
              base_write
            ),
          }
        end
      end
    end
  end
  if base_input == 0 and base_output == 0 then
    return nil
  end
  local cost = {
    input = base_input,
    output = base_output,
    cache_read = base_read,
    cache_write = base_write,
  }
  if #tiers > 0 then
    cost.tiers = tiers
  end
  return cost
end

-- Whether the service's API list holds `want`: a value that is not a list
-- holds nothing.
local function has_api(apis, want)
  if type(apis) ~= "table" then
    return false
  end
  for _, api in ipairs(apis) do
    if api == want then
      return true
    end
  end
  return false
end

-- The entry for one model service, or nil to leave it out: kept only when
-- its API list holds the Responses or chat API, and left out when its bare
-- name ends `-image` or matches an `image_generation` row. A Claude
-- service, by its row's `provider_type` or its `claude-` name, is
-- `anthropic-messages` with `compat.bearer`; else a Responses service is
-- `openai-responses`, else `openai-completions`. The window is the row's
-- `max_input`, except the pair 922000/128000 which reads as 1,050,000; a
-- service with no row, or a row with no numeric `max_input`, has no window.
local function entry_for(service, catalog, image_keys)
  if type(service) ~= "table" or type(service.name) ~= "string" then
    return nil
  end
  local bare = service.name:gsub("^model%-services/system%.ai%.", "")
  if bare == "" then
    return nil
  end
  local responses = has_api(service.supported_api_types, "openai/v1/responses")
  if not responses and not has_api(service.supported_api_types, "mlflow/v1/chat/completions") then
    return nil
  end
  local row = metadata_for(bare, catalog)
  local claude = bare:sub(1, 7) == "claude-"
  if type(row) == "table" and row.provider_type == "ANTHROPIC" then
    claude = true
  end
  if bare:sub(-6) == "-image" or image_keys[model_key(bare)] then
    return nil
  end
  local protocol, base_path
  if claude then
    protocol = "anthropic-messages"
    base_path = "/ai-gateway/anthropic/v1"
  elseif responses then
    protocol = "openai-responses"
    base_path = "/ai-gateway/codex/v1"
  else
    protocol = "openai-completions"
    base_path = "/ai-gateway/mlflow/v1"
  end
  local entry = {
    id = "databricks-" .. bare,
    protocol = protocol,
    base_url = SCHEME .. "{workspace}" .. base_path,
  }
  if claude then
    entry.compat = { bearer = true }
  end
  local vision = type(row) == "table"
    and type(row.capabilities) == "table"
    and row.capabilities.vision
  if vision then
    entry.input = { "text", "image" }
  else
    entry.input = { "text" }
  end
  if type(row) == "table" and type(row.context_window) == "table" then
    local window = row.context_window
    local max_in = number(window.max_input)
    if max_in ~= nil then
      if max_in == 922000 and number(window.max_output) == 128000 then
        entry.context_window = 1050000
      else
        entry.context_window = max_in
      end
      local max_out = number(window.max_output) or number(window.max_tokens)
      if max_out ~= nil then
        entry.max_output_tokens = max_out
      end
    end
  end
  if type(row) == "table" then
    local cost = cost_from(row.base_pricing)
    if cost ~= nil then
      entry.cost = cost
    end
  end
  return entry
end

fiber.provider("databricks", {
  models = {
    timeout = 30000,
    run = function(arg)
      if type(arg.key) ~= "string" then
        error("databricks models: no token; set DATABRICKS_TOKEN or run `fiber login databricks`", 0)
      end
      local placeholders = arg.placeholders
      local workspace = type(placeholders) == "table" and placeholders.workspace or nil
      if type(workspace) ~= "string" then
        error("databricks models: no workspace host", 0)
      end
      local services = pages(workspace, arg.key, SERVICES_PATH, SERVICES_QUERY, SERVICES_FIELD)
      local catalog = pages(workspace, arg.key, CATALOG_PATH, CATALOG_QUERY, CATALOG_FIELD)
      local image_keys = {}
      for _, row in ipairs(catalog) do
        if
          type(row) == "table"
          and row.mode == "image_generation"
          and type(row.model_name) == "string"
        then
          image_keys[model_key(row.model_name)] = true
        end
      end
      local list = {}
      for _, service in ipairs(services) do
        local entry = entry_for(service, catalog, image_keys)
        if entry ~= nil then
          list[#list + 1] = entry
        end
      end
      table.sort(list, function(a, b)
        return a.id < b.id
      end)
      return list
    end,
  },
})
