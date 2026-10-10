-- One anonymous frame drives the queue using the engine's monotonic GetTime clock.
-- The callback snapshot defers newly scheduled timers until the following frame.
local frame = CreateFrame("Frame")
local reportError = ...
local timers = {}
local Handle = {}
function Handle:Cancel() self.cancelled = true; self.callback = nil end
function Handle:IsCancelled() return self.cancelled == true end

local function update()
  local due, pending = {}, {}
  local now = GetTime()
  for _, timer in ipairs(timers) do
    if not timer.cancelled then
      if now >= timer.deadline then table.insert(due, timer)
      else table.insert(pending, timer) end
    end
  end
  timers = pending
  for _, timer in ipairs(due) do
    if not timer.cancelled then
      local callback = timer.callback
      if timer.iterations then
        timer.iterations = timer.iterations - 1
      end
      if timer.iterations == 0 then
        timer:Cancel()
      else
        timer.deadline = now + timer.period
        table.insert(timers, timer)
      end
      local success, message = pcall(callback, timer)
      if not success then reportError(tostring(message)) end
    end
  end
  if table.getn(timers) == 0 then frame:SetScript("OnUpdate", nil) end
end

local function schedule(seconds, callback, iterations)
  assert(type(seconds) == "number" and seconds == seconds and seconds < math.huge and seconds > -math.huge,
    "timer duration must be finite")
  assert(type(callback) == "function", "timer callback must be a function")
  local period = math.max(seconds, 0.0001)
  local timer = Mixin({period = period, deadline = GetTime() + math.max(seconds, 0),
    callback = callback, iterations = iterations}, Handle)
  table.insert(timers, timer)
  frame:SetScript("OnUpdate", update)
  return timer
end
C_Timer = {}
function C_Timer.After(seconds, callback)
  schedule(seconds, function() callback() end, 1)
end
function C_Timer.NewTimer(seconds, callback)
  return schedule(seconds, callback, 1)
end
function C_Timer.NewTicker(seconds, callback, iterations)
  if type(iterations) ~= "number" then iterations = nil
  else
    assert(iterations >= 1 and iterations < math.huge, "ticker iterations must be positive")
    iterations = math.floor(iterations)
  end
  return schedule(seconds, callback, iterations)
end
function RunNextFrame(callback) C_Timer.After(0, callback) end
