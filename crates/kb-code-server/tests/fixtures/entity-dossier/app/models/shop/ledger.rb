# frozen_string_literal: true

module Shop
  class Ledger
    if Rails.env.production?
      private
    end

    def post
      true
    end
  end
end
