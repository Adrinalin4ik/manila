-- A minimap button and /hd for manila's `hdPatches` setting: whether the lettered patch files
-- (patch-A.mpq to patch-Z.mpq, where HD packs install) load. The client reads the setting when
-- it starts, so a change needs a restart; writing 1 to the `restartClient` setting does that.

-- What this session started with. The setting itself may be changed since.
local active = GetCVar("hdPatches") ~= "0"

local function restart()
  SetCVar("restartClient", "1")
end

local function wanted()
  return GetCVar("hdPatches") ~= "0"
end

local function word(on)
  return on and "|cff33ff33On|r" or "|cffff5555Off|r"
end

local function say(text)
  DEFAULT_CHAT_FRAME:AddMessage("|cff33ffccHD Switch:|r " .. text)
end

StaticPopupDialogs["MANILA_HD_RESTART"] = {
  text = "%s",
  button1 = "Restart now",
  button2 = "Later",
  OnAccept = restart,
  timeout = 0,
  whileDead = 1,
  hideOnEscape = 1,
}

local button = CreateFrame("Button", "ManilaHDSwitchButton", Minimap)
button:SetWidth(31)
button:SetHeight(31)
button:SetFrameStrata("MEDIUM")
button:SetFrameLevel(8)
button:RegisterForClicks("LeftButtonUp", "RightButtonUp")
button:SetHighlightTexture("Interface\\Minimap\\UI-Minimap-ZoomButton-Highlight")
-- On the rim, lower left, where the stock buttons leave room.
button:SetPoint("TOPLEFT", Minimap, "TOPLEFT", 52 - 80 * math.cos(math.rad(215)), 80 * math.sin(math.rad(215)) - 52)

local icon = button:CreateTexture(nil, "BACKGROUND")
icon:SetWidth(20)
icon:SetHeight(20)
icon:SetPoint("TOPLEFT", 7, -5)
icon:SetTexture("Interface\\Icons\\INV_Misc_Spyglass_03")
icon:SetTexCoord(0.07, 0.93, 0.07, 0.93)

local border = button:CreateTexture(nil, "OVERLAY")
border:SetWidth(53)
border:SetHeight(53)
border:SetPoint("TOPLEFT", 0, 0)
border:SetTexture("Interface\\Minimap\\MiniMap-TrackingBorder")

local function refresh()
  -- Bright while the packs are loaded, dimmed without them; tinted while a change is pending.
  if wanted() ~= active then
    icon:SetVertexColor(1, 0.8, 0.2)
  elseif active then
    icon:SetVertexColor(1, 1, 1)
  else
    icon:SetVertexColor(0.45, 0.45, 0.45)
  end
end

local function tooltip()
  GameTooltip:SetOwner(button, "ANCHOR_LEFT")
  GameTooltip:AddLine("HD Patches")
  GameTooltip:AddDoubleLine("Loaded now:", word(active), 1, 1, 1)
  if wanted() ~= active then
    GameTooltip:AddDoubleLine("After a restart:", word(wanted()), 1, 1, 1)
    GameTooltip:AddLine("Right-click to restart now.", 0.6, 0.6, 0.6)
  end
  GameTooltip:AddLine("Left-click to switch.", 0.6, 0.6, 0.6)
  GameTooltip:Show()
end

local function set(on)
  SetCVar("hdPatches", on and "1" or "0")
  refresh()
  if on ~= active then
    StaticPopup_Show("MANILA_HD_RESTART", "HD patches will be " .. (on and "on" or "off") .. " after a restart.")
  else
    StaticPopup_Hide("MANILA_HD_RESTART")
    say("staying " .. word(active) .. ".")
  end
end

button:SetScript("OnClick", function()
  if arg1 == "RightButton" then
    if wanted() ~= active then restart() else say("nothing to apply, HD patches are " .. word(active) .. ".") end
  else
    set(not wanted())
  end
  if GameTooltip:IsOwned(button) then tooltip() end
end)
button:SetScript("OnEnter", tooltip)
button:SetScript("OnLeave", function() GameTooltip:Hide() end)
-- The options page writes the same setting; follow it.
button:SetScript("OnShow", refresh)
button:RegisterEvent("CVAR_UPDATE")
button:SetScript("OnEvent", refresh)
refresh()

SLASH_MANILAHD1 = "/hd"
SlashCmdList["MANILAHD"] = function(msg)
  msg = string.lower(msg or "")
  if msg == "on" then
    set(true)
  elseif msg == "off" then
    set(false)
  elseif msg == "restart" then
    restart()
  elseif msg == "" or msg == "toggle" then
    set(not wanted())
  else
    say("loaded now " .. word(active) .. ", after a restart " .. word(wanted()) .. ". Use /hd, /hd on, /hd off or /hd restart.")
  end
end
