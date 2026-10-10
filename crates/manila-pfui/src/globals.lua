-- ClassicAPI globals pfUI calls directly. Each reads the engine's own state or stock queries.

function IsModifierKeyDown()
  if IsShiftKeyDown() or IsControlKeyDown() or IsAltKeyDown() then return 1 end
end

-- `chars` is a literal character set, never a pattern.
function strtrim(text, chars)
  assert(type(text) == 'string' or type(text) == 'number', 'strtrim requires a string')
  text = tostring(text)
  chars = chars or ' \t\n\v\f\r'
  local first, last = 1, #text
  while first <= last and chars:find(text:sub(first, first), 1, true) do first = first + 1 end
  while last >= first and chars:find(text:sub(last, last), 1, true) do last = last - 1 end
  return text:sub(first, last)
end

-- The ID companions return the item the stock link builder already names for the same slot.
local function linked(link)
  if type(link) ~= 'string' then return end
  local id = link:match('|Hitem:(%d+)')
  return id and tonumber(id)
end
for name, source in pairs({
  GetLootRollItemID = 'GetLootRollItemLink',
  GetLootSlotItemID = 'GetLootSlotLink',
  GetMerchantItemID = 'GetMerchantItemLink',
  GetQuestItemID = 'GetQuestItemLink',
  GetQuestLogItemID = 'GetQuestLogItemLink',
  GetTradeSkillItemID = 'GetTradeSkillItemLink',
  GetTradeSkillReagentItemID = 'GetTradeSkillReagentItemLink',
  GetCraftReagentItemID = 'GetCraftReagentItemLink',
}) do
  _G[name] = function(...) return linked(_G[source](...)) end
end

-- The spell a macro shows: its #showtooltip/#show argument, else its first cast. Only a name
-- the player's books resolve is returned; conditionals and items resolve to nothing.
local function shown(body)
  local first
  for line in body:gmatch('[^\r\n]+') do
    line = strtrim(line)
    local directive, argument = line:match('^#(%a+)%s*(.*)$')
    if directive then
      directive = directive:lower()
      if (directive == 'showtooltip' or directive == 'show') and argument ~= '' then
        return argument
      end
    elseif not first then
      local command, rest = line:match('^/(%a+)%s+(.+)$')
      command = command and command:lower()
      if command == 'cast' then
        first = rest
      elseif command == 'castsequence' then
        rest = rest:gsub('^reset=%S+%s*', '')
        first = rest:match('^([^,]+)')
      else
        first = line:match('CastSpellByName%(%s*"([^"]+)"') or
          line:match("CastSpellByName%(%s*'([^']+)'")
      end
    end
  end
  return first
end
function GetMacroSpell(slot)
  local _, _, body = GetMacroInfo(slot)
  if type(body) ~= 'string' or not GetSpellInfo then return end
  local wanted = shown(body)
  if not wanted then return end
  local name, rank, _, _, _, _, _, _, _, id = GetSpellInfo(strtrim(wanted))
  if name then return name, rank, id end
end

-- Tracking spells are the player's book entries whose DBC effect applies a tracking aura:
-- SPELL_AURA_TRACK_CREATURES (44), TRACK_RESOURCES (45) or TRACK_STEALTHED (151).
local TRACKING = { [44] = true, [45] = true, [151] = true }
local function trackers()
  local result = {}
  if not GetSpellInfo then return result end
  local slot = 1
  while GetSpellName(slot, 'spell') do
    local name, _, _, _, _, _, _, _, _, id = GetSpellInfo(slot, 'spell')
    local effects = id and C_Spell.GetSpellEffectInfo(id)
    if effects then
      for _, effect in ipairs(effects) do
        if effect.effect ~= 0 and TRACKING[effect.auraName] then
          result[#result + 1] = { slot = slot, name = name, id = id }
          break
        end
      end
    end
    slot = slot + 1
  end
  return result
end
function GetNumTrackingTypes() return #trackers() end
function GetTrackingInfo(index)
  local tracker = trackers()[index]
  if not tracker then return end
  local texture = GetSpellTexture(tracker.slot, 'spell')
  -- One tracker is in effect at a time, and the stock query names it by its icon.
  return tracker.name, texture, texture ~= nil and texture == GetTrackingTexture(), 'spell', tracker.id
end
function SetTracking(index)
  local tracker = trackers()[index]
  if tracker then CastSpell(tracker.slot, 'spell') end
end

-- Packed item locations, as C_EquipmentSet.GetItemLocations publishes them.
ITEM_INVENTORY_LOCATION_PLAYER = 0x00100000
ITEM_INVENTORY_LOCATION_BAGS = 0x00200000
ITEM_INVENTORY_LOCATION_BANK = 0x00400000
function EquipmentManager_UnpackLocation(location)
  if type(location) ~= 'number' or location < 0 then return false, false, false, 0 end
  local flags = math.floor(location / 0x00100000)
  local player, bags, bank = flags % 2 == 1, math.floor(flags / 2) % 2 == 1, math.floor(flags / 4) % 2 == 1
  local rest = location % 0x00100000
  if not bags then return player, bank, false, rest % 256 end
  local bag = math.floor(rest / 256) % 256
  if bank then bag = bag + 4 end
  return player, bank, true, rest % 256, bag
end
function EquipmentManager_GetLocationData(location)
  local player, bank, bags, slot, bag = EquipmentManager_UnpackLocation(location)
  return { isPlayer = player, isBank = bank, isBags = bags, slot = slot, bag = bag }
end

-- Run before each addon loads: the stock colour tables exist by then, and FrameXML assigns them
-- as plain tables after this extension installs.
local bridge = ...
function bridge.beforeAddon()
  for name, value in pairs(_G) do
    if type(name) == 'string' and type(value) == 'table' and name:find('_FONT_COLOR$') and
      type(value.r) == 'number' and value.GetRGB == nil then
      if value.a == nil then value.a = 1 end
      Mixin(value, ColorMixin)
    end
  end
  if WHITE_FONT_COLOR == nil then WHITE_FONT_COLOR = CreateColor(1, 1, 1) end
end

-- Inline texture markup as later clients spell it. 1.12's text engine draws the escape
-- literally, so the caller's string is correct while the icon itself is not rendered.
function CreateSimpleTextureMarkup(file, width, height)
  return string.format('|T%s:%d:%d|t', file, height or width, width)
end
