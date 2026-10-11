-- OpenCode Go's quota (docs/tools.md, "Where it comes from"): the percent
-- used of Go's rolling, weekly and monthly windows, with reset times, from
-- GET https://opencode.ai/zen/go/v1/usage for the label's key. A call
-- without a key makes no request and reports nothing, and a reply naming
-- none of the three windows reports nothing. Anything else unreadable
-- raises, so the cache reports `unreachable`: a status other than 200, a
-- body that is not a JSON object, a `usage` field that is not a table, a
-- window that is present without being null yet not a table, or a present
-- window whose `percent` is not a number at or above 0 or whose
-- `resetsAt` is not a UTC timestamp of the form `2026-10-01T13:18:56.498Z`
-- with each field in range and a day the calendar holds, leap years
-- included. JSON null decodes to a light userdata, never nil, so every
-- read here tests the type. A window's `status` is ignored: only "ok" was
-- ever measured (research/opencode-probe/README.md).
--
-- Zen reports no quota, so this package declares nothing for it.

-- Days since the Unix epoch for the civil date `year`-`month`-`day`:
-- Howard Hinnant's days-from-civil in exact integer arithmetic.
local function days_from_civil(year, month, day)
  if month <= 2 then
    year = year - 1
    month = month + 12
  end
  local era = math.floor(year / 400)
  local yoe = year - era * 400
  local doy = math.floor((153 * (month - 3) + 2) / 5) + day - 1
  local doe = yoe * 365 + math.floor(yoe / 4) - math.floor(yoe / 100) + doy
  return era * 146097 + doe - 719468
end

-- Whether `year` is a leap year of the proleptic Gregorian calendar: every
-- fourth year holds February 29th except a century year outside every
-- four-hundredth.
local function is_leap_year(year)
  return year % 4 == 0 and (year % 100 ~= 0 or year % 400 == 0)
end

-- The Unix seconds of the UTC timestamp `text`, of the form
-- `2026-10-01T13:18:56.498Z`. Raises when the text is not of that form, a
-- field is out of range, or the day is not on the calendar: February holds
-- 29 days in a leap year, 28 otherwise. The fraction drops: the pattern
-- keeps exactly three digits and reads the time in whole seconds.
local function resets_at_epoch(text, name)
  if type(text) ~= "string" then
    error("opencode quota: the " .. name .. " window holds no reset time", 0)
  end
  local year, month, day, hour, min, sec =
    text:match("^(%d%d%d%d)%-(%d%d)%-(%d%d)T(%d%d):(%d%d):(%d%d)%.%d%d%dZ$")
  if year == nil then
    error("opencode quota: the " .. name .. " window holds no reset time", 0)
  end
  year, month, day, hour, min, sec =
    tonumber(year), tonumber(month), tonumber(day), tonumber(hour), tonumber(min), tonumber(sec)
  if month < 1 or month > 12 or hour > 23 or min > 59 or sec > 59 then
    error("opencode quota: the " .. name .. " window holds no reset time", 0)
  end
  -- The day the month holds: days_from_civil would normalise an impossible
  -- date into the next month, so February 31st must raise here. February
  -- holds 29 days in a leap year, 28 otherwise.
  local month_days = { 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31 }
  local last_day = month_days[month]
  if month == 2 and is_leap_year(year) then
    last_day = 29
  end
  if day < 1 or day > last_day then
    error("opencode quota: the " .. name .. " window holds no reset time", 0)
  end
  return days_from_civil(year, month, day) * 86400 + hour * 3600 + min * 60 + sec
end

-- One usage window of the `usage` table `seen`: the entry `name` becomes
-- the Fiber window of the same name, or nil when the entry is absent or
-- null. A present entry that is not a table raises, as does one whose
-- `percent` is not a number at or above 0 or whose `resetsAt` is not a
-- timestamp of the form above. JSON null decodes to a light userdata,
-- never nil, so the entry read tests the type.
local function usage_window(seen, name)
  local window = seen[name]
  if window == nil or type(window) == "userdata" then
    return nil
  end
  if type(window) ~= "table" then
    error("opencode quota: the " .. name .. " window holds no usage", 0)
  end
  if type(window.percent) ~= "number" or window.percent < 0 then
    error("opencode quota: the " .. name .. " window holds no percent used", 0)
  end
  return {
    name = name,
    used_percent = window.percent,
    resets_at = resets_at_epoch(window.resetsAt, name),
  }
end

fiber.provider("opencode-go", {
  quota = { timeout = 8000, run = function(call)
    if call.key == nil then
      return nil
    end
    local reply = host.http({
      url = "https://opencode.ai/zen/go/v1/usage",
      headers = { authorization = "Bearer " .. call.key },
    })
    if reply.status ~= 200 then
      error("opencode quota: unexpected status " .. tostring(reply.status), 0)
    end
    local ok, body = pcall(json.decode, reply.body)
    -- A JSON array decodes to a Lua table too: only a `{` after any space
    -- opens the object the usage reply is.
    if not ok or type(body) ~= "table" or reply.body:match("^%s*(.)") ~= "{" then
      error("opencode quota: the usage reply is not a JSON object", 0)
    end
    local seen = body.usage
    if type(seen) ~= "table" then
      error("opencode quota: the usage reply holds no usage", 0)
    end
    local windows = {}
    for _, name in ipairs({ "rolling", "weekly", "monthly" }) do
      local window = usage_window(seen, name)
      if window ~= nil then
        windows[#windows + 1] = window
      end
    end
    if #windows == 0 then
      return nil
    end
    return { windows = windows }
  end },
})
