-- The fixture provider reads its test-local server address from fixture.url.
local function hex(bytes)
  return (bytes:gsub(".", function(c) return string.format("%02x", c:byte()) end))
end

fiber.provider("fixture", {
  models = {
    timeout = 5000,
    run = function()
      local base = host.secret("fixture.url")
      local reply = host.http({
        url = base .. "/v1/models",
        headers = { authorization = "Bearer " .. host.secret("fixture.api_key") },
      })
      local list = {}
      for _, m in ipairs(json.decode(reply.body).data) do
        list[#list + 1] = {
          id = m.id,
          protocol = "openai-responses",
          base_url = base .. "/v1",
          context_window = m.context_length,
        }
      end
      return list
    end,
  },
  credential = {
    timeout = 5000,
    run = function()
      local reply = host.http({
        method = "POST",
        url = host.secret("fixture.url") .. "/token",
        headers = { ["content-type"] = "application/json" },
        body = json.encode({ key = host.secret("fixture.api_key") }),
      })
      local t = json.decode(reply.body)
      return { token = t.access_token, expires_at = t.expires_at }
    end,
  },
  -- Signs the method, URL and body hash, and reports which fields it saw.
  sign = {
    timeout = 30000,
    run = function(request)
      local seen = {}
      for key in pairs(request) do seen[#seen + 1] = key end
      table.sort(seen)
      local text = request.method .. "\n" .. request.url .. "\n" .. request.body_sha256
      return {
        ["x-fixture-signature"] = hex(host.hmac_sha256("fixture-secret", text)),
        ["x-fixture-content-sha256"] = request.body_sha256,
        ["x-fixture-saw"] = table.concat(seen, ","),
      }
    end,
  },
})
