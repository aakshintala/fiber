fiber.provider("muse", {
  models = {
    timeout = 10000,
    run = function(call)
      local reply = host.http({
        method = "GET",
        url = "https://api.meta.ai/v1/models",
        headers = { authorization = "Bearer " .. call.key },
      })
      if reply.status ~= 200 then
        error("muse models: unexpected status " .. tostring(reply.status), 0)
      end
      local ok, body = pcall(json.decode, reply.body)
      if not ok then
        error("muse models: the reply body is not JSON", 0)
      end
      if type(body) ~= "table" or type(body.data) ~= "table" then
        error("muse models: the reply holds no model list", 0)
      end
      local models = {}
      for _, item in ipairs(body.data) do
        if type(item) == "table" and type(item.id) == "string" then
          models[#models + 1] = {
            id = item.id,
            protocol = "openai-responses",
            base_url = "https://api.meta.ai/v1",
          }
        end
      end
      return models
    end,
  },
})
