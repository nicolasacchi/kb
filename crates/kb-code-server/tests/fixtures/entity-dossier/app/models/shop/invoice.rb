# frozen_string_literal: true

module Shop
  class Invoice < Order
    include Payable
  end
end
