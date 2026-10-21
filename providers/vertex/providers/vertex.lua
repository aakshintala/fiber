-- Google Vertex (docs/model-routing.md, "Protocols and providers"): an OAuth
-- access token from the Google Cloud CLI, which Fiber sends as
-- `authorization: Bearer` and refreshes shortly before it expires
-- (docs/model-routing.md, "Keys, tokens and OAuth").
--
-- The session's credential label names a gcloud configuration. Under any
-- label but `default`, credential() takes the token of that configuration's
-- account: `gcloud config config-helper --configuration <label>`. Under
-- `default` it takes the active configuration's, and only when gcloud has
-- no account selected at all, the token of Application Default
-- Credentials: `gcloud auth application-default print-access-token`. Any
-- other failure, such as a login that needs reauthentication, fails rather
-- than switching to another account.
--
-- Each command's `--format` prints only the token and its expiry, so no
-- other field, such as the account or a refresh token, reaches Fiber. gcloud
-- keeps a token until 5 minutes before it expires, as Fiber does, so a
-- refresh gets a new one. A token never appears in an error, and gcloud's
-- own failure line has its account and anything token-like masked.

local HELPER = "config config-helper"
local HELPER_FORMAT = "--format=json(credential.access_token,credential.token_expiry)"
local ADC = "auth application-default print-access-token"
local ADC_FORMAT = "--format=json(token,expiry.datetime)"

-- What gcloud says when no account is selected at all
-- (`NoActiveAccountException` in its SDK): the one failure Application
-- Default Credentials stand in for.
local NO_ACCOUNT = "You do not currently have an active account selected"

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

-- The number of days in month `m` of year `y`.
local function days_in_month(y, m)
  if m == 2 then
    return (y % 4 == 0 and (y % 100 ~= 0 or y % 400 == 0)) and 29 or 28
  end
  return (m == 4 or m == 6 or m == 9 or m == 11) and 30 or 31
end

-- Unix seconds of an expiry as gcloud prints it: `2026-10-20T17:41:19Z`
-- from `config-helper`, or `2026-10-20 18:00:51.894905` from
-- `application-default print-access-token`, which is UTC with no zone, as
-- google-auth keeps it. An offset such as `+00:00` is read too. Nil for
-- anything else, including a field out of range, so a bad expiry fails
-- rather than putting off the refresh.
local function unix_seconds(text)
  if type(text) ~= "string" then
    return nil
  end
  local y, mo, d, h, mi, s, rest =
    text:match("^(%d%d%d%d)-(%d%d)-(%d%d)[T ](%d%d):(%d%d):(%d%d)(.*)$")
  if y == nil then
    return nil
  end
  y, mo, d = tonumber(y), tonumber(mo), tonumber(d)
  h, mi, s = tonumber(h), tonumber(mi), tonumber(s)
  if mo < 1 or mo > 12 or d < 1 or d > days_in_month(y, mo)
    or h > 23 or mi > 59 or s > 59
  then
    return nil
  end
  rest = rest:gsub("^%.%d+", "")
  local offset = 0
  if rest ~= "" and rest ~= "Z" then
    local sign, oh, om = rest:match("^([+-])(%d%d):?(%d%d)$")
    if sign == nil or tonumber(oh) > 23 or tonumber(om) > 59 then
      return nil
    end
    offset = (tonumber(oh) * 60 + tonumber(om)) * 60
    if sign == "-" then
      offset = -offset
    end
  end
  return days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s - offset
end

-- The line of gcloud's stderr that says what failed: its first `ERROR:`
-- line, without the `ERROR: (gcloud.<command>)` that names the command
-- again, else its first line that is not blank. A `WARNING:` before it can
-- name a file under the person's home. gcloud can name the account, as in
-- "Your current active account [alice@example.com] does not have any valid
-- credentials", and no credential is known yet to redact a secret with, so
-- an address becomes `[account]` and each run of 20 or more token-like
-- characters `[redacted]`; then the line stops at 200 bytes.
local function failure_line(stderr)
  local text = stderr or ""
  local line = text:match("ERROR:[^\r\n]*") or text:match("[^\r\n]*[^%s][^\r\n]*") or ""
  line = line:gsub("^ERROR: %([%w%.%-]+%) ", "")
  line = line:gsub("%[?[%w%.%%%+_%-]+@[%w%.%-]+%]?", "[account]")
  line = line:gsub("[%w%+/=%._%-]+", function(run)
    if #run >= 20 then
      return "[redacted]"
    end
  end)
  if #line > 200 then
    line = line:sub(1, 200) .. "..."
  end
  return line
end

-- Runs `gcloud <what> <format>`, with `--configuration <configuration>`
-- when one is named, and returns what it printed, decoded. On failure,
-- nil, why, how it failed (`missing` when gcloud could not be run at all,
-- `exit` when it exited non-zero, `output` when what it printed is not
-- JSON), and its stderr when it exited non-zero.
local function gcloud(what, format, configuration)
  local args = {}
  for word in what:gmatch("%S+") do
    args[#args + 1] = word
  end
  args[#args + 1] = format
  if configuration ~= nil then
    args[#args + 1] = "--configuration"
    args[#args + 1] = configuration
  end
  local ok, ran = pcall(host.exec, "gcloud", args)
  if not ok then
    local why = type(ran) == "table" and ran.message or tostring(ran)
    return nil, "the Google Cloud CLI (`gcloud`) could not be run (" .. why .. ")", "missing"
  end
  if ran.exit_code ~= 0 then
    local first = failure_line(ran.stderr)
    return nil, "`gcloud " .. what .. "` failed (exit " .. tostring(ran.exit_code) .. ")"
      .. (first ~= "" and (": " .. first) or ""), "exit", ran.stderr or ""
  end
  local parsed, body = pcall(json.decode, ran.stdout)
  if not parsed or type(body) ~= "table" then
    return nil, "`gcloud " .. what .. "` printed something other than JSON", "output"
  end
  return body
end

-- The credential() return for `token` and `expiry`, as `what` printed
-- them.
local function returned(token, expiry, what)
  if type(token) ~= "string" or token == "" then
    error("vertex credential: `gcloud " .. what .. "` printed no access token", 0)
  end
  local expires_at = unix_seconds(expiry)
  if expires_at == nil then
    error("vertex credential: `gcloud " .. what .. "` printed no expiry that is a UTC time", 0)
  end
  return { token = token, expires_at = expires_at }
end

fiber.provider("vertex", {
  credential = { timeout = 30000, run = function(arg)
    -- The credential label names the gcloud configuration; `default` takes
    -- the active one, then Application Default Credentials.
    local configuration = arg.label
    if type(configuration) ~= "string" or configuration == "" or configuration == "default" then
      configuration = nil
    end
    local body, why, failed, stderr = gcloud(HELPER, HELPER_FORMAT, configuration)
    if body ~= nil then
      local credential = type(body.credential) == "table" and body.credential or {}
      return returned(credential.access_token, credential.token_expiry, HELPER)
    end
    if failed == "missing" then
      error("vertex credential: " .. why .. "; install it and run `gcloud auth login`", 0)
    end
    if failed == "output" then
      error("vertex credential: " .. why, 0)
    end
    -- gcloud ran and found no token: under a named configuration that is
    -- the answer. Under `default`, Application Default Credentials stand in
    -- only when gcloud has no account selected at all; any other failure,
    -- such as a login that needs reauthentication, keeps to the account
    -- gcloud names rather than switching to whichever one ADC holds.
    if configuration ~= nil then
      error("vertex credential: " .. why .. "; the credential label names a gcloud"
        .. " configuration, and `gcloud config configurations list` lists them", 0)
    end
    if not stderr:find(NO_ACCOUNT, 1, true) then
      error("vertex credential: " .. why .. "; run `gcloud auth login` to log in again", 0)
    end
    local adc, adc_why = gcloud(ADC, ADC_FORMAT, nil)
    if adc ~= nil then
      local expiry = type(adc.expiry) == "table" and adc.expiry.datetime or nil
      return returned(adc.token, expiry, ADC)
    end
    error("vertex credential: " .. why .. "; " .. adc_why .. "; run `gcloud auth login`"
      .. " or `gcloud auth application-default login`, or name a gcloud configuration"
      .. " as the credential label, `providers.\"vertex\".credential`", 0)
  end },
})
