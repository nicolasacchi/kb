# frozen_string_literal: true

module Shop
  module Payable
    def pay
      true
    end

    def refund
      false
    end

    private

    def gateway
      nil
    end
  end
end
