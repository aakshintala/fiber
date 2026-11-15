fiber.provider("gemini", {
  models = {
    timeout = 10000,
    run = function(call)
      local function encode(value)
        return (value:gsub("[^A-Za-z0-9%-_%.~]", function(c)
          return string.format("%%%02X", string.byte(c))
        end))
      end
      local function supports_content(methods)
        if type(methods) ~= "table" then
          return false
        end
        for _, method in ipairs(methods) do
          if method == "generateContent" then
            return true
          end
        end
        return false
      end

      local models = {}
      local page_token = nil
      while true do
        local url = "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000"
        if page_token ~= nil then
          url = url .. "&pageToken=" .. encode(page_token)
        end
        local reply = host.http({
          method = "GET",
          url = url,
          headers = { ["x-goog-api-key"] = call.key },
        })
        if reply.status ~= 200 then
          error("gemini models: unexpected status " .. tostring(reply.status), 0)
        end
        local ok, page = pcall(json.decode, reply.body)
        if not ok then
          error("gemini models: the reply body is not JSON", 0)
        end
        if type(page) ~= "table" or type(page.models) ~= "table" then
          error("gemini models: the reply holds no model list", 0)
        end
        for _, item in ipairs(page.models) do
          if type(item) == "table"
            and type(item.name) == "string"
            and item.name:sub(1, 7) == "models/"
            and supports_content(item.supportedGenerationMethods)
          then
            models[#models + 1] = {
              id = item.name:sub(8),
              protocol = "google-generative-ai",
              base_url = "https://generativelanguage.googleapis.com/v1beta",
            }
          end
        end
        if type(page.nextPageToken) ~= "string" or page.nextPageToken == "" then
          return models
        end
        page_token = page.nextPageToken
      end
    end,
  },
})
