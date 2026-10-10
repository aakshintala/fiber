-- OpenRouter's model list (docs/model-routing.md, "Model discovery"): one
-- entry per model of GET https://openrouter.ai/api/v1/models whose
-- `supported_parameters` include `tools`. The endpoint needs no key, so the
-- request carries no authorization. Any failure raises, so the cached list
-- stays: a status other than 200, a body that is not JSON, a `data` field
-- that is not a table, an unreadable price, or a list that would hold no
-- entry at all.
--
-- OpenRouter's generation lookup (docs/model-routing.md, "Cost"): the cost of
-- a call that ended without one, from GET <base_url>/generation?id=<id>.
-- The lookup answers 404 until the generation's cost is known, which is
-- nothing, not an error.

-- Every byte outside A-Z, a-z, 0-9 and -_.~ becomes %XX.
local function encode(text)
  return (text:gsub("[^A-Za-z0-9%-_%.~]", function(c)
    return string.format("%%%02X", string.byte(c))
  end))
end

-- Whether the decoded JSON list `list` holds `want`. A value that is not
-- a table holds nothing: JSON null decodes to a light userdata, never nil,
-- so every read here tests the type.
local function contains(list, want)
  if type(list) ~= "table" then
    return false
  end
  for _, v in ipairs(list) do
    if v == want then
      return true
    end
  end
  return false
end

-- The price `pricing[field]` in US dollars per million tokens. OpenRouter
-- quotes dollars per token as a string, so appending `e6` shifts the
-- decimal: `0.0000001` reads as `0.1`, where multiplying would round. A
-- value that is absent, null or not a string counts as absent, as does a
-- negative price. A string no number reads from raises.
local function price_per_million(pricing, field)
  local raw = pricing[field]
  if type(raw) ~= "string" then
    return nil
  end
  local per_m = tonumber(raw .. "e6")
  if per_m == nil then
    error("openrouter models: unreadable price for `" .. field .. "`: " .. raw, 0)
  end
  if per_m < 0 then
    return nil
  end
  return per_m
end

fiber.provider("openrouter", {
  models = {
    timeout = 10000,
    run = function()
      local reply = host.http({
        url = "https://openrouter.ai/api/v1/models",
      })
      if reply.status ~= 200 then
        error("openrouter models: unexpected status " .. tostring(reply.status), 0)
      end
      local data = json.decode(reply.body).data
      if type(data) ~= "table" then
        error("openrouter models: the reply holds no model list", 0)
      end
      local list = {}
      for _, m in ipairs(data) do
        if type(m) == "table" and contains(m.supported_parameters, "tools") then
          local entry = {
            id = m.id,
            protocol = "openai-completions",
            base_url = "https://openrouter.ai/api/v1",
            compat = { cache_key_field = "session_id" },
          }
          if contains(m.supported_parameters, "reasoning") then
            entry.compat.reasoning_object = true
          end
          -- The thinking levels the model takes: its reported efforts
          -- kept in Fiber order, or OpenRouter's normalized effort values
          -- when it reports reasoning but no efforts. A model without
          -- reasoning takes none.
          local levels = nil
          local reasoning = m.reasoning
          if type(reasoning) == "table" and type(reasoning.supported_efforts) == "table" then
            levels = {}
            for _, level in ipairs({ "minimal", "low", "medium", "high", "xhigh", "max" }) do
              if contains(reasoning.supported_efforts, level) then
                levels[#levels + 1] = level
              end
            end
            if #levels == 0 then
              levels = nil
            end
          end
          if levels ~= nil then
            entry.thinking_levels = levels
            if contains(levels, reasoning.default_effort) then
              entry.thinking_default = reasoning.default_effort
            end
          elseif contains(m.supported_parameters, "reasoning") then
            -- debt: reasoning without efforts stays [low,medium,high], replace
            -- when OpenRouter reports per-model efforts for it (provisional:
            -- OpenRouter documents low/medium/high as its normalized values).
            entry.thinking_levels = { "low", "medium", "high" }
          end
          if type(m.id) == "string" and m.id:sub(1, 10) == "anthropic/" then
            entry.compat.anthropic = true
          end
          if type(m.context_length) == "number" then
            entry.context_window = m.context_length
          end
          if type(m.top_provider) == "table"
            and type(m.top_provider.max_completion_tokens) == "number"
          then
            entry.max_output_tokens = m.top_provider.max_completion_tokens
          end
          if type(m.architecture) == "table"
            and type(m.architecture.input_modalities) == "table"
          then
            entry.input = m.architecture.input_modalities
          end
          if type(m.pricing) == "table" then
            local input = price_per_million(m.pricing, "prompt")
            local output = price_per_million(m.pricing, "completion")
            if input ~= nil and output ~= nil then
              local cost = { input = input, output = output }
              local cache_read = price_per_million(m.pricing, "input_cache_read")
              if cache_read ~= nil then
                cost.cache_read = cache_read
              end
              local cache_write = price_per_million(m.pricing, "input_cache_write")
              if cache_write ~= nil then
                cost.cache_write = cache_write
              end
              entry.cost = cost
            end
          end
          list[#list + 1] = entry
        end
      end
      if #list == 0 then
        error("openrouter models: the listing holds no model with `tools`", 0)
      end
      return list
    end,
  },
  cost = {
    timeout = 10000,
    run = function(call)
      local headers = {}
      if call.key ~= nil then
        headers.authorization = "Bearer " .. call.key
      end
      local reply = host.http({
        url = call.base_url .. "/generation?id=" .. encode(call.generation_id),
        headers = headers,
      })
      if reply.status ~= 200 then
        return nil
      end
      local data = json.decode(reply.body).data
      if type(data) ~= "table" then
        return nil
      end
      if type(data.total_cost) == "number" then
        return data.total_cost
      end
      return nil
    end,
  },
})
