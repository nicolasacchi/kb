# frozen_string_literal: true

module Trade
  class RoundsController < ApplicationController
    def show
      @round = Round.find(params[:id])
    end

    def offers_tab
      render partial: 'offers_tab_content', layout: false
    end

    def row_preview
      render partial: 'row'
    end

    def dynamic
      render partial: computed_partial_name
    end

    private

    def computed_partial_name
      'row'
    end
  end
end
