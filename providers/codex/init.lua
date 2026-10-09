-- ChatGPT/codex's login (docs/model-routing.md, "Logging in"): a browser
-- login with PKCE, or a device code the person types, then a refresh of the
-- stored credential. The token reply's access token carries the account id
-- and its expiry; the id token carries the email, read only while logging
-- in. What is stored is the token, its expiry, the refresh token and the
-- account id, nothing else. The wire constants are pi's flow and the codex
-- CLI's strings (research/codex-responses-probe/README.md, "Login").

local AUTH = "https://auth.openai.com"
local PORT = 1455
local CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann"
local SCOPE = "openid profile email offline_access"

-- Every byte outside A-Z, a-z, 0-9 and -_.~ becomes %XX.
local function encode(text)
  return (text:gsub("[^A-Za-z0-9%-_%.~]", function(c)
    return string.format("%%%02X", string.byte(c))
  end))
end

-- `fields` as `application/x-www-form-urlencoded`, in order.
local function form(fields)
  local parts = {}
  for _, field in ipairs(fields) do
    parts[#parts + 1] = encode(field[1]) .. "=" .. encode(field[2])
  end
  return table.concat(parts, "&")
end

local BASE64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"

-- Base64url without padding, as a JWT's parts use. Nil for a character
-- outside the alphabet.
local function base64url_decode(text)
  local digits = {}
  for i = 1, #text do
    local at = BASE64:find(text:sub(i, i), 1, true)
    if at == nil then
      return nil
    end
    digits[#digits + 1] = at - 1
  end
  local bytes = {}
  local bits, held = 0, 0
  for _, digit in ipairs(digits) do
    -- Six bits at a time, flushed every byte: the value stays small enough
    -- for exact arithmetic.
    bits = bits * 64 + digit
    held = held + 6
    while held >= 8 do
      held = held - 8
      bytes[#bytes + 1] = string.char(math.floor(bits / 2 ^ held) % 256)
      bits = bits % 2 ^ held
    end
  end
  return table.concat(bytes)
end

-- The JSON claims of `token`'s payload. Nil when it is not three
-- dot-separated base64url parts with a JSON object payload.
local function claims(token)
  if type(token) ~= "string" then
    return nil
  end
  local middle = token:match("^[^%.]+%.([^%.]+)%.[^%.]*$")
  if middle == nil then
    return nil
  end
  local decoded = base64url_decode(middle)
  if decoded == nil then
    return nil
  end
  local ok, payload = pcall(json.decode, decoded)
  if not ok or type(payload) ~= "table" then
    return nil
  end
  return payload
end

-- Raises the login or refresh failure: a string in a login, which the login
-- reports as `credential_failed`, and a table in a refresh, whose code the
-- refresh mapping keeps as a rejected refresh (docs/model-routing.md,
-- "Keys, tokens and OAuth").
local function fail(stored, message)
  if stored == nil then
    error("codex login: " .. message, 0)
  end
  error({ code = "authentication_failed", message = "codex: " .. message }, 0)
end

-- The token reply's stored fields: the access token, its expiry, the
-- refresh token and the account id. The email is read only while logging
-- in, from the id token.
local function read_token_reply(reply, stored, email)
  if reply.status ~= 200 then
    fail(stored, "the token endpoint answered status " .. tostring(reply.status))
  end
  local ok, body = pcall(json.decode, reply.body)
  if not ok or type(body) ~= "table" then
    fail(stored, "the token reply is not a JSON object")
  end
  if type(body.access_token) ~= "string" or body.access_token == "" then
    fail(stored, "the token reply held no access token")
  end
  if type(body.refresh_token) ~= "string" or body.refresh_token == "" then
    fail(stored, "the token reply held no refresh token")
  end
  local access = claims(body.access_token)
  local auth = access ~= nil and access["https://api.openai.com/auth"]
  local account_id = type(auth) == "table" and auth.chatgpt_account_id or nil
  if type(account_id) ~= "string" or account_id == "" then
    fail(stored, "the access token holds no account id")
  end
  if access == nil or math.type(access.exp) ~= "integer" then
    fail(stored, "the access token holds no expiry")
  end
  if stored == nil and type(body.id_token) == "string" then
    local id = claims(body.id_token)
    local address = id ~= nil and id.email or nil
    if type(address) == "string" and address ~= "" then
      email.address = address
    end
  end
  return {
    token = body.access_token,
    expires_at = access.exp,
    refresh_token = body.refresh_token,
    account_id = account_id,
  }
end

-- POSTs the form `fields` to the token endpoint.
local function post_token(fields)
  return host.http({
    url = AUTH .. "/oauth/token",
    method = "POST",
    headers = { ["content-type"] = "application/x-www-form-urlencoded" },
    body = form(fields),
  })
end

-- The browser login: PKCE and a state, the authorize URL opened for the
-- person, the code from the callback, and the exchange. A callback that
-- fails on a bound port names device login as the way out.
local function browser_login(email)
  local pkce = host.oauth.pkce()
  local state = host.oauth.pkce().verifier
  local redirect = "http://localhost:" .. tostring(PORT) .. "/auth/callback"
  host.oauth.open(AUTH .. "/oauth/authorize"
    .. "?response_type=code"
    .. "&client_id=" .. CLIENT_ID
    .. "&redirect_uri=" .. encode(redirect)
    .. "&scope=" .. encode(SCOPE)
    .. "&code_challenge=" .. pkce.challenge
    .. "&code_challenge_method=S256"
    .. "&state=" .. state
    .. "&id_token_add_organizations=true"
    .. "&codex_cli_simplified_flow=true"
    .. "&originator=fiber")
  local ok, query = pcall(host.oauth.callback, { port = PORT, path = "/auth/callback" })
  if not ok then
    local message = tostring(query)
    if message:find("port " .. tostring(PORT), 1, true) then
      error("codex login: port " .. tostring(PORT)
        .. " is in use (is the codex CLI logging in?);"
        .. " log in with `fiber login codex --device`", 0)
    end
    error("codex login: the browser callback failed: " .. message, 0)
  end
  if type(query.error) == "string" then
    error("codex login: the browser login was refused (" .. query.error .. ")", 0)
  end
  if query.state ~= state then
    error("codex login: the browser callback state did not match", 0)
  end
  if type(query.code) ~= "string" or query.code == "" then
    error("codex login: the browser callback held no code", 0)
  end
  return read_token_reply(post_token({
    { "grant_type", "authorization_code" },
    { "client_id", CLIENT_ID },
    { "code", query.code },
    { "code_verifier", pkce.verifier },
    { "redirect_uri", redirect },
  }), nil, email)
end

-- The device login: a user code shown for the person to enter, the pending
-- polls, and the exchange with the verifier the poll returned.
local function device_login(email)
  local reply = host.http({
    url = AUTH .. "/api/accounts/deviceauth/usercode",
    method = "POST",
    headers = { ["content-type"] = "application/json" },
    body = json.encode({ client_id = CLIENT_ID }),
  })
  if reply.status ~= 200 then
    fail(nil, "the device code request failed with status " .. tostring(reply.status))
  end
  local ok, started = pcall(json.decode, reply.body)
  if not ok
    or type(started) ~= "table"
    or type(started.device_auth_id) ~= "string"
    or type(started.user_code) ~= "string"
  then
    fail(nil, "the device code reply is not usable")
  end
  -- The interval arrives as a string or a number; anything else waits the
  -- poll's own five seconds.
  local interval = started.interval
  if type(interval) == "string" then
    interval = tonumber(interval)
  end
  if math.type(interval) ~= "integer" or interval < 1 then
    interval = 5
  end
  if interval > 3600 then
    interval = 3600
  end
  host.oauth.show(AUTH .. "/codex/device", started.user_code)
  local polled = host.oauth.poll({
    url = AUTH .. "/api/accounts/deviceauth/token",
    method = "POST",
    headers = { ["content-type"] = "application/json" },
    body = json.encode({ device_auth_id = started.device_auth_id, user_code = started.user_code }),
    pending = { 403, 404 },
    interval = interval,
  })
  if type(polled.authorization_code) ~= "string"
    or type(polled.code_verifier) ~= "string"
  then
    fail(nil, "the device token reply held no authorization code")
  end
  return read_token_reply(post_token({
    { "grant_type", "authorization_code" },
    { "client_id", CLIENT_ID },
    { "code", polled.authorization_code },
    { "code_verifier", polled.code_verifier },
    { "redirect_uri", AUTH .. "/deviceauth/callback" },
  }), nil, email)
end

-- The session's refresh: one form POST with the stored refresh token. A
-- 400 or 401 raises the rejected-refresh table; an endpoint that cannot be
-- reached propagates `host.http`'s own failure.
local function refresh_session(stored)
  if type(stored.refresh_token) ~= "string" or stored.refresh_token == "" then
    fail(stored, "the stored credential holds no refresh token")
  end
  local reply = post_token({
    { "grant_type", "refresh_token" },
    { "refresh_token", stored.refresh_token },
    { "client_id", CLIENT_ID },
  })
  if reply.status == 400 or reply.status == 401 then
    error({
      code = "authentication_failed",
      message = "codex: the token endpoint refused the refresh"
        .. " (status " .. tostring(reply.status) .. ")",
    }, 0)
  end
  return read_token_reply(reply, stored, {})
end

fiber.provider("codex", {
  credential = { timeout = 900000, run = function(arg)
    local email = {}
    local result = host.oauth.refresh(function(stored)
      if stored == nil then
        if arg.login == "device" then
          return device_login(email)
        end
        return browser_login(email)
      end
      return refresh_session(stored)
    end)
    -- The stored value holds no headers: every return rebuilds them from
    -- the stored account id, so a request never pairs a token with another
    -- token's headers (docs/model-routing.md, "Keys, tokens and OAuth").
    local account_id = result.account_id
    if type(account_id) ~= "string" or account_id == "" then
      error("codex credential: the stored credential holds no account id;"
        .. " run `fiber login codex` again", 0)
    end
    local returned = {
      token = result.token,
      expires_at = result.expires_at,
      headers = { ["chatgpt-account-id"] = account_id },
    }
    if email.address ~= nil then
      returned.email = email.address
    end
    return returned
  end },
})

