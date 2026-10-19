-- Claude on AWS Bedrock (docs/model-routing.md, "Protocols and providers"):
-- a Bedrock API key, sent as `authorization: Bearer`, or AWS credentials,
-- with which `sign()` signs each request with SigV4 in place of the bearer
-- token.
--
-- The session's credential label names the AWS profile. Under any label
-- but `default`, `credential()` takes what `aws configure
-- export-credentials --profile <label> --format process` prints, so a
-- profile, SSO or an assumed role works with no AWS SDK. Under `default`
-- it looks in this order, and the first found wins:
--
-- 1. a Bedrock API key in `AWS_BEARER_TOKEN_BEDROCK`;
-- 2. AWS keys in `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`, with
--    `AWS_SESSION_TOKEN` when set;
-- 3. what `aws configure export-credentials --format process` prints for
--    the AWS CLI's own default chain, which reads `AWS_PROFILE`.
--
-- Credentials with an `Expiration` are used until Fiber refreshes them,
-- shortly before it (docs/model-routing.md, "Keys, tokens and OAuth").
--
-- Lua reads no environment, so one `sh` prints the four variables,
-- separated by NUL, which no environment value holds. AWS credentials ride
-- to `sign()` as the bearer token's JSON; `sign()` replaces that header
-- with the SigV4 one, so they are never sent. A key or credential never
-- appears in an error.
--
-- `sign()` is AWS Signature Version 4: the region and service come from the
-- host, `<prefix>.<region>.amazonaws.com`, where `bedrock-runtime` signs as
-- `bedrock`; every header the request carries is signed except
-- `authorization` and `user-agent`, with `host`, `x-amz-date` and
-- `x-amz-security-token`; the path is percent-encoded once more, as for
-- every AWS service but S3; a request that already carries `host` or
-- `x-amz-date` is signed with it, as AWS's SigV4 test suite gives them.

local NEVER = 253402300799 -- 9999-12-31T23:59:59Z: a key that does not expire.
local ALGORITHM = "AWS4-HMAC-SHA256"

local ENV_SCRIPT = 'printf "%s\\000%s\\000%s\\000%s" '
  .. '"$AWS_BEARER_TOKEN_BEDROCK" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" "$AWS_SESSION_TOKEN"'

-- The four variables, each "" when unset.
local function environment()
  local ran = host.exec("sh", { "-c", ENV_SCRIPT })
  if ran.exit_code ~= 0 then
    error("bedrock credential: `sh` could not read the environment", 0)
  end
  local values = {}
  for value in (ran.stdout .. "\0"):gmatch("([^\0]*)\0") do
    values[#values + 1] = value
  end
  return {
    api_key = values[1] or "",
    access_key_id = values[2] or "",
    secret_access_key = values[3] or "",
    session_token = values[4] or "",
  }
end

-- Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
-- `days_from_civil`).
local function days_from_civil(y, m, d)
  if m <= 2 then
    y = y - 1
  end
  local era = (y >= 0 and y or y - 399) // 400
  local yoe = y - era * 400
  local doy = (153 * (m > 2 and m - 3 or m + 9) + 2) // 5 + d - 1
  local doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
  return era * 146097 + doe - 719468
end

-- The date of a day count since 1970-01-01 (`civil_from_days`).
local function civil_from_days(z)
  z = z + 719468
  local era = (z >= 0 and z or z - 146096) // 146097
  local doe = z - era * 146097
  local yoe = (doe - doe // 1460 + doe // 36524 - doe // 146096) // 365
  local y = yoe + era * 400
  local doy = doe - (365 * yoe + yoe // 4 - yoe // 100)
  local mp = (5 * doy + 2) // 153
  local d = doy - (153 * mp + 2) // 5 + 1
  local m = mp < 10 and mp + 3 or mp - 9
  if m <= 2 then
    y = y + 1
  end
  return y, m, d
end

-- Unix seconds of an ISO 8601 time as the AWS CLI prints `Expiration`,
-- such as `2026-10-20T03:43:31+00:00` or `2026-10-20T03:43:31Z`, with or
-- without fractional seconds. Nil for anything else.
local function unix_seconds(text)
  if type(text) ~= "string" then
    return nil
  end
  local y, mo, d, h, mi, s, rest =
    text:match("^(%d%d%d%d)-(%d%d)-(%d%d)T(%d%d):(%d%d):(%d%d)(.*)$")
  if y == nil then
    return nil
  end
  rest = rest:gsub("^%.%d+", "")
  local offset
  if rest == "Z" or rest == "" then
    offset = 0
  else
    local sign, oh, om = rest:match("^([+-])(%d%d):?(%d%d)$")
    if sign == nil then
      return nil
    end
    offset = (tonumber(oh) * 60 + tonumber(om)) * 60
    if sign == "-" then
      offset = -offset
    end
  end
  local days = days_from_civil(tonumber(y), tonumber(mo), tonumber(d))
  return days * 86400 + tonumber(h) * 3600 + tonumber(mi) * 60 + tonumber(s) - offset
end

-- The AWS CLI's credentials for `profile`, or for its own default chain
-- when `profile` is nil.
local function exported(profile)
  local args = { "configure", "export-credentials", "--format", "process" }
  if profile ~= nil then
    args[#args + 1] = "--profile"
    args[#args + 1] = profile
  end
  local ok, ran = pcall(host.exec, "aws", args)
  if not ok then
    local why = type(ran) == "table" and ran.message or tostring(ran)
    error("bedrock credential: the AWS CLI (`aws`) could not be run (" .. why .. ");"
      .. " install it, or set AWS_BEARER_TOKEN_BEDROCK or AWS_ACCESS_KEY_ID and"
      .. " AWS_SECRET_ACCESS_KEY", 0)
  end
  if ran.exit_code ~= 0 then
    local first = (ran.stderr or ""):match("[^\r\n]*[^%s][^\r\n]*") or ""
    error("bedrock credential: `aws configure export-credentials` failed"
      .. " (exit " .. tostring(ran.exit_code) .. ")"
      .. (first ~= "" and (": " .. first) or ""), 0)
  end
  local parsed, body = pcall(json.decode, ran.stdout)
  if not parsed
    or type(body) ~= "table"
    or type(body.AccessKeyId) ~= "string"
    or body.AccessKeyId == ""
    or type(body.SecretAccessKey) ~= "string"
    or body.SecretAccessKey == ""
  then
    error("bedrock credential: `aws configure export-credentials` printed no access key", 0)
  end
  local expires_at = NEVER
  if body.Expiration ~= nil then
    expires_at = unix_seconds(body.Expiration)
    if expires_at == nil then
      error("bedrock credential: `aws configure export-credentials` printed an"
        .. " `Expiration` that is not an ISO 8601 time", 0)
    end
  end
  local session_token = type(body.SessionToken) == "string" and body.SessionToken or ""
  return {
    access_key_id = body.AccessKeyId,
    secret_access_key = body.SecretAccessKey,
    session_token = session_token,
  }, expires_at
end

-- The value of header `name` in `headers`, whatever its case.
local function header(headers, name)
  for key, value in pairs(headers) do
    if key:lower() == name then
      return value
    end
  end
  return nil
end

local function hex(bytes)
  return (bytes:gsub(".", function(c)
    return string.format("%02x", c:byte())
  end))
end

-- Every byte outside A-Z, a-z, 0-9 and -_.~ as %XX, and `/` too unless
-- `keep_slash`.
local function encode(text, keep_slash)
  return (text:gsub("[^A-Za-z0-9%-_%.~/]", function(c)
    return string.format("%%%02X", c:byte())
  end):gsub("/", keep_slash and "/" or "%%2F"))
end

-- The canonical query: each `name=value` pair as it was sent, sorted by
-- name, then value.
local function canonical_query(query)
  if query == "" then
    return ""
  end
  local pairs_ = {}
  for pair in query:gmatch("[^&]+") do
    local name, value = pair:match("^([^=]*)=?(.*)$")
    pairs_[#pairs_ + 1] = { name, value }
  end
  table.sort(pairs_, function(a, b)
    if a[1] ~= b[1] then
      return a[1] < b[1]
    end
    return a[2] < b[2]
  end)
  local parts = {}
  for _, pair in ipairs(pairs_) do
    parts[#parts + 1] = pair[1] .. "=" .. pair[2]
  end
  return table.concat(parts, "&")
end

-- `time` (Unix seconds) as SigV4's `20150830T123600Z`.
local function amz_date(time)
  local y, m, d = civil_from_days(time // 86400)
  local seconds = time % 86400
  return string.format("%04d%02d%02dT%02d%02d%02dZ",
    y, m, d, seconds // 3600, seconds % 3600 // 60, seconds % 60)
end

-- The headers SigV4 adds to `arg`'s request, signed with `credentials`.
local function sigv4(arg, credentials)
  local authority, path, query =
    arg.url:match("^%a[%w+.-]*://([^/?#]*)([^?#]*)%??([^#]*)")
  if authority == nil then
    error("bedrock sign: the request URL has no host", 0)
  end
  authority = authority:gsub("^.*@", ""):gsub(":443$", "")
  local hostname = authority:gsub(":%d+$", ""):lower()
  local prefix, region = hostname:match("^([^.]+)%.([^.]+)%.amazonaws%.com$")
  if prefix == nil then
    error("bedrock sign: " .. hostname .. " is not an AWS endpoint naming its region", 0)
  end
  local service = prefix == "bedrock-runtime" and "bedrock" or prefix

  local signed = {}
  for name, value in pairs(arg.headers) do
    local lower = name:lower()
    if lower ~= "authorization" and lower ~= "user-agent" then
      signed[lower] = value
    end
  end
  local added = {}
  if signed.host == nil then
    signed.host = authority
  end
  local date = signed["x-amz-date"]
  if date == nil then
    date = amz_date(arg.time)
    signed["x-amz-date"] = date
    added["x-amz-date"] = date
  end
  if credentials.session_token ~= "" and signed["x-amz-security-token"] == nil then
    signed["x-amz-security-token"] = credentials.session_token
    added["x-amz-security-token"] = credentials.session_token
  end

  local names = {}
  for name in pairs(signed) do
    names[#names + 1] = name
  end
  table.sort(names)
  local lines = {}
  for _, name in ipairs(names) do
    local value = signed[name]:gsub("^%s+", ""):gsub("%s+$", ""):gsub("%s+", " ")
    lines[#lines + 1] = name .. ":" .. value .. "\n"
  end
  local signed_headers = table.concat(names, ";")
  local canonical = table.concat({
    arg.method:upper(),
    encode(path == "" and "/" or path, true),
    canonical_query(query),
    table.concat(lines),
    signed_headers,
    arg.body_sha256,
  }, "\n")

  local day = date:sub(1, 8)
  local scope = day .. "/" .. region .. "/" .. service .. "/aws4_request"
  local to_sign = table.concat({ ALGORITHM, date, scope, host.sha256(canonical) }, "\n")
  local key = "AWS4" .. credentials.secret_access_key
  for _, part in ipairs({ day, region, service, "aws4_request" }) do
    key = host.hmac_sha256(key, part)
  end
  added.authorization = ALGORITHM
    .. " Credential=" .. credentials.access_key_id .. "/" .. scope
    .. ", SignedHeaders=" .. signed_headers
    .. ", Signature=" .. hex(host.hmac_sha256(key, to_sign))
  return added
end

fiber.provider("bedrock", {
  credential = { timeout = 30000, run = function(arg)
    -- The credential label names the AWS profile; `default` takes the
    -- environment, then the AWS CLI's own default chain.
    local profile = arg.label
    if type(profile) ~= "string" or profile == "" or profile == "default" then
      profile = nil
    end
    if profile ~= nil then
      local credentials, expires_at = exported(profile)
      return { token = json.encode(credentials), expires_at = expires_at }
    end
    local env = environment()
    if env.api_key ~= "" then
      return { token = env.api_key, expires_at = NEVER }
    end
    local credentials, expires_at
    if env.access_key_id ~= "" and env.secret_access_key ~= "" then
      credentials = {
        access_key_id = env.access_key_id,
        secret_access_key = env.secret_access_key,
        session_token = env.session_token,
      }
      expires_at = NEVER
    else
      credentials, expires_at = exported(nil)
    end
    return { token = json.encode(credentials), expires_at = expires_at }
  end },
  sign = { timeout = 2000, run = function(arg)
    local authorization = header(arg.headers, "authorization")
    local token = type(authorization) == "string" and authorization:match("^Bearer (.*)$") or nil
    -- A Bedrock API key goes as it is; only AWS credentials are signed with.
    if token == nil or token:sub(1, 1) ~= "{" then
      return {}
    end
    local ok, credentials = pcall(json.decode, token)
    if not ok
      or type(credentials) ~= "table"
      or type(credentials.access_key_id) ~= "string"
      or type(credentials.secret_access_key) ~= "string"
    then
      error("bedrock sign: the credential is not a set of AWS keys", 0)
    end
    if type(credentials.session_token) ~= "string" then
      credentials.session_token = ""
    end
    return sigv4(arg, credentials)
  end },
})
