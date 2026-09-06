# frozen_string_literal: true

class RowComponent < ViewComponent::Base
  def initialize(item:)
    @item = item
  end
end
