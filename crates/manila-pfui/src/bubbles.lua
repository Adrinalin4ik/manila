-- Frames follow the host's active speech bubbles, including hidden offscreen speakers.
local occupied, free, live = {}, {}, {}
C_ChatBubbles = {}
function C_ChatBubbles.GetAllChatBubbles()
  local copy = {}
  for index, frame in ipairs(live) do copy[index] = frame end
  return copy
end
local function make()
  local frame = CreateFrame('Frame', nil, WorldFrame)
  frame:Hide()
  frame:SetFrameStrata('BACKGROUND')
  frame:EnableMouse(false)
  local text = frame:CreateFontString(nil, 'OVERLAY', 'GameFontNormal')
  text:SetJustifyH('CENTER')
  text:SetJustifyV('CENTER')
  text:SetPoint('CENTER', frame, 'CENTER')
  local tail = frame:CreateTexture(nil, 'ARTWORK')
  tail:SetTexture('Interface\\Tooltips\\ChatBubble-Tail')
  tail:SetPoint('TOPRIGHT',frame,'BOTTOM',0,0)
  return { frame=frame, text=text, tail=tail }
end
return function(rows)
  local seen={}
  live={}
  for _,row in ipairs(rows) do seen[row.guid]=true end
  -- Retire before allocating so an expired speaker's frame can be reused in this batch.
  for guid,bubble in pairs(occupied) do
    if not seen[guid] then
      bubble.frame:Hide()
      occupied[guid]=nil
      free[#free+1]=bubble
    end
  end
  for rank, row in ipairs(rows) do
    local bubble=occupied[row.guid]
    if not bubble then
      bubble=table.remove(free) or make()
      occupied[row.guid]=bubble
    end
    local frame, text = bubble.frame, bubble.text
    local fresh = bubble.guid ~= row.guid or bubble.born ~= row.born
    if fresh or not row.visible then frame:Hide() end
    bubble.guid, bubble.born = row.guid, row.born
    if text:GetText() ~= row.text then text:SetText(row.text) end
    text:SetTextColor(row.r,row.g,row.b,1)
    if row.visible then
      local ownerScale = WorldFrame and WorldFrame:GetEffectiveScale() or 1
      local font = text:GetFont()
      if not bubble.initialized then
        text:SetFont(font or STANDARD_TEXT_FONT, row.fontHeight / ownerScale)
        bubble.initialized=true
        frame:SetBackdrop({bgFile='Interface\\Tooltips\\ChatBubble-Background',edgeFile='Interface\\Tooltips\\ChatBubble-Backdrop',edgeSize=row.border / ownerScale,insets={left=row.border / ownerScale,right=row.border / ownerScale,top=row.border / ownerScale,bottom=row.border / ownerScale}})
      end
      text:SetWidth(0)
      local width=math.max(2*row.border / ownerScale,math.min(text:GetStringWidth(), row.wrap / ownerScale))
      text:SetWidth(width)
      text:SetHeight(0)
      local height=text:GetHeight()
      frame:SetSize(width+2*row.margin / ownerScale,height+2*row.margin / ownerScale)
      bubble.tail:SetSize(row.border / ownerScale,row.border / ownerScale)
      frame:SetFrameLevel(rank+1)
      frame:SetAlpha(row.alpha)
      frame:ClearAllPoints()
      frame:SetPoint('BOTTOM',WorldFrame,'BOTTOMLEFT',row.x / ownerScale,row.bottom / ownerScale)
      -- pfUI hides the source text and installs a copy. Refresh that copy through its OnShow
      -- whenever the same speaker replaces a line, including when this frame is recycled.
      frame:Show()
    end
    live[#live+1]=frame
  end
end
